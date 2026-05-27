//! Top-level state for the import wizard. The detailed handling lives in
//! sibling submodules — `wizard` owns key/async logic, `render` owns tick
//! and every draw call, `worker` hosts the standalone import thread.

mod dup_detect;
mod render;
mod wizard;
mod worker;

pub use dup_detect::{Conflict, ConflictDecision};

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use rusqlite::Connection;

use crate::core::tagger;
use crate::db::models::Track;
use crate::external::matching::MatchScore;
use crate::external::musicbrainz::{MbClient, MbRelease};
use crate::tui::widgets::confirm_delete::ConfirmDelete;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::pick_collection::PickCollectionPopup;

#[derive(Debug, Clone, PartialEq)]
pub enum ImportStep {
    SelectSource,
    Scanning,
    Review,
    /// Reached after the review summary when duplicate detection finds
    /// conflicts. User picks a side per conflict, then moves on to
    /// `Importing`. Skipped entirely when there are no conflicts.
    ResolveDuplicates,
    Importing,
    Complete,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MbMatchState {
    NotStarted,
    Searching,
    Done,
    /// MB search threw an error (HTTP failure, rate-limit, JSON parse, etc).
    /// String is the short reason to surface in the UI; full detail goes to
    /// the log.
    Failed(String),
}

#[derive(Clone)]
pub struct MbCandidate {
    pub release: MbRelease,
    pub score: MatchScore,
}

#[derive(Clone)]
pub struct ImportGroup {
    pub name: String,
    pub tracks: Vec<(Track, Option<tagger::TagData>)>,
    pub action: GroupAction,
    pub mb_candidates: Vec<MbCandidate>,
    pub selected_candidate: Option<usize>,
    pub mb_state: MbMatchState,
    /// If non-empty, all tracks in this group will be added to this collection
    /// (created if it doesn't exist) during the Importing step.
    pub target_collection: String,
    /// `true` while a background `fetch_release` is in flight for the
    /// currently selected candidate. Used to avoid firing a second fetch
    /// before the first lands. Reset in the `release_fetch_rx` drain.
    pub full_release_fetching: bool,
    /// Set when the user has made an explicit decision for this group
    /// (pressed A/S/L/0, picked a candidate with 1-9, or arrowed the
    /// selection). Blocks the MB auto-accept in the result drain from
    /// clobbering that decision if the search finishes afterwards.
    pub user_decided: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum GroupAction {
    AcceptAsIs,
    AcceptMb,
    Skip,
    Loose,
}

impl ImportGroup {
    /// Tag-uniformity test for AcceptAsIs groups: does the group describe
    /// a single coherent album, or is it a mixed bag (compilation,
    /// hand-picked singles, collection-bound bundle)? Mirrors the worker's
    /// `group_is_real_album` so the dup detector and the importer agree
    /// on which groups even *have* an album-slot space to dedupe against.
    ///
    /// Returns `true` only when:
    ///   1. Every track shares the same `(album, album_artist)` tuple.
    ///   2. The album_artist isn't a "various" marker.
    ///   3. Track-level artists aren't wildly diverse (>= 4 distinct, or
    ///      >= 40% unique).
    ///
    /// Caller is responsible for excluding Loose / MB / Skip groups —
    /// this only checks the tag shape.
    pub fn has_consistent_album_tags(&self) -> bool {
        let key = |td: &Option<crate::core::tagger::TagData>| -> Option<(String, Option<String>)> {
            td.as_ref().and_then(|t| {
                t.album.as_ref().map(|a| {
                    (
                        a.clone(),
                        t.album_artist.clone().or_else(|| t.artist.clone()),
                    )
                })
            })
        };
        let first_key = self.tracks.first().and_then(|(_, td)| key(td));
        let consistent = first_key.is_some()
            && self.tracks.iter().all(|(_, td)| key(td) == first_key);
        if !consistent {
            return false;
        }

        let album_artist_lower = first_key
            .as_ref()
            .and_then(|(_, aa)| aa.as_ref())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let is_various_marker = matches!(
            album_artist_lower.as_str(),
            "various"
                | "various artists"
                | "various artist"
                | "va"
                | "v.a."
                | "compilation"
                | "compilations"
                | "soundtrack"
                | "ost"
        );
        if is_various_marker {
            return false;
        }

        use std::collections::HashSet;
        let unique_artists: HashSet<String> = self
            .tracks
            .iter()
            .filter_map(|(_, td)| td.as_ref().and_then(|t| t.artist.as_ref()).cloned())
            .collect();
        let n_tracks = self.tracks.len().max(1);
        // The 40% rule needs enough tracks to be meaningful — for a 1-3
        // track EP every artist is by definition a high percentage. Gate
        // it on `n_tracks >= 4` so small legitimate releases (singles,
        // EPs) don't get misclassified as compilations.
        let too_diverse = unique_artists.len() >= 4
            || (n_tracks >= 4 && (unique_artists.len() * 100 / n_tracks) >= 40);
        !too_diverse
    }
}

pub struct ImportView {
    pub step: ImportStep,
    pub source_paths: Vec<PathBuf>,
    /// Index into `source_paths` that points at the configured `music_dir`,
    /// if it was appended as a source. Used by the renderer to tag that row
    /// as "(library)" so the user knows why it's being scanned — every import
    /// pass audits the library dir for files the DB doesn't know about.
    pub library_source_index: Option<usize>,
    pub groups: Vec<ImportGroup>,
    pub current_group: usize,
    pub scan_progress: (usize, usize),
    pub import_progress: (usize, usize),
    pub result_summary: Option<String>,
    // SelectSource step state
    pub custom_path: TextInput,
    pub use_custom_path: bool,
    pub custom_path_error: Option<String>,
    // MB rate limit from config
    pub rate_limit_ms: u64,
    /// Script preference for MB-derived artist/album names. Passed to the
    /// shared `MbClient` and to the import worker so both apply the same
    /// resolution on search-result fetches and on per-group commits.
    pub name_script: crate::config::settings::NameScriptPreference,
    /// Whether the import worker should mirror MB-matched metadata to the
    /// audio file's tags. Sourced from `[tagging] write_tags` in config.
    pub write_tags: bool,
    /// Threshold above which the top MB candidate is auto-accepted on
    /// arrival. Sourced from `[import] auto_match_threshold`. Lives here
    /// (rather than re-fetched from settings on each render) because the
    /// auto-accept runs in the per-tick result drain.
    pub auto_match_threshold: f64,
    /// Number of MB candidates fetched per group during search. Sourced
    /// from `[import] match_candidates`.
    pub match_candidates: u32,
    /// Resolved DB path threaded into the background import worker. Lives
    /// here so the worker, which spawns its own connection on its own
    /// thread, doesn't have to re-derive it from settings.
    pub db_path: PathBuf,
    scan_rx: Option<mpsc::Receiver<ScanMessage>>,
    /// Persistent MB search channel. Lives across all searches so a prefetch
    /// of the *next* group can complete even after the user navigates and we
    /// kick off a fresh search for the new current group.
    mb_rx: Option<mpsc::Receiver<MbResult>>,
    mb_tx: Option<mpsc::Sender<MbResult>>,
    /// Shared MB client — `Arc<Mutex<_>>` so concurrent search threads
    /// serialize through the client's internal throttler instead of
    /// racing against MB's rate limit with separate clients.
    mb_client: Option<Arc<Mutex<MbClient>>>,
    /// Manual MBID input mode during Review.
    mbid_input: Option<TextInput>,
    /// Receives a fetched release from a manual MBID lookup.
    mbid_fetch_rx: Option<mpsc::Receiver<MbResult>>,
    /// Channel for background `fetch_release` calls that populate a
    /// selected candidate's tracklist ahead of dup detection. Separate
    /// from `mb_rx` because the semantics differ: these results update
    /// one existing candidate in place instead of replacing the list.
    release_fetch_rx: Option<mpsc::Receiver<ReleaseFetchResult>>,
    release_fetch_tx: Option<mpsc::Sender<ReleaseFetchResult>>,
    /// Per-group collection picker (during Review). When `Some`, captures input.
    collection_picker: Option<PickCollectionPopup>,
    /// Receives progress messages from the background importer.
    import_rx: Option<mpsc::Receiver<ImportMessage>>,
    /// Pending duplicate conflicts detected between review and import.
    /// `decisions[i]` is the user's choice for `conflicts[i]`; `cursor`
    /// is the one currently on screen. Empty when no conflicts (the
    /// wizard skips `ResolveDuplicates` entirely in that case).
    pub(super) conflicts: Vec<Conflict>,
    pub(super) decisions: Vec<ConflictDecision>,
    pub(super) conflict_cursor: usize,
    /// Cancel-confirmation popup shown when the user presses `q` or `Esc`
    /// during an active import wizard. `quit_on_confirm` is `true` when the
    /// user pressed `q` (leave wizard + quit app) vs `false` for `Esc`
    /// (leave wizard, return to library).
    pub confirm_cancel: Option<ConfirmCancel>,
}

/// State for the import cancel-confirmation popup.
pub struct ConfirmCancel {
    pub popup: ConfirmDelete,
    /// `true` → quit the app after leaving the wizard (`q`).
    /// `false` → return to library (`Esc`).
    pub quit_on_confirm: bool,
}

enum ScanMessage {
    Progress(usize, usize),
    Complete(Vec<ImportGroup>),
}

/// Result from a single-group MB search on the background thread.
struct MbResult {
    group_idx: usize,
    candidates: Vec<MbCandidate>,
    /// If the search itself failed (HTTP / parse), a short reason for the UI.
    /// `None` means the call succeeded, even if it returned zero candidates.
    error: Option<String>,
}

/// Messages from the background import thread.
enum ImportMessage {
    Progress(usize, usize),
    Complete(String),
}

/// Result from a background `fetch_release` whose only purpose is to
/// populate a candidate's tracklist (so MBID-based duplicate detection
/// has something to key on). The release MBID is echoed back so we can
/// line up the result with the right candidate even if the user changed
/// their selection while the fetch was in flight.
pub(super) struct ReleaseFetchResult {
    pub(super) group_idx: usize,
    pub(super) release_mbid: String,
    pub(super) release: Option<MbRelease>,
}

impl Default for ImportView {
    fn default() -> Self {
        Self {
            step: ImportStep::SelectSource,
            source_paths: Vec::new(),
            library_source_index: None,
            groups: Vec::new(),
            current_group: 0,
            scan_progress: (0, 0),
            import_progress: (0, 0),
            result_summary: None,
            custom_path: TextInput::new("~/Music/new-album").with_label(" Path: "),
            use_custom_path: false,
            custom_path_error: None,
            rate_limit_ms: 1100,
            name_script: crate::config::settings::NameScriptPreference::Native,
            write_tags: true,
            auto_match_threshold: 0.85,
            match_candidates: 5,
            db_path: PathBuf::new(),
            scan_rx: None,
            mb_rx: None,
            mb_tx: None,
            mb_client: None,
            mbid_input: None,
            mbid_fetch_rx: None,
            release_fetch_rx: None,
            release_fetch_tx: None,
            collection_picker: None,
            import_rx: None,
            conflicts: Vec::new(),
            decisions: Vec::new(),
            conflict_cursor: 0,
            confirm_cancel: None,
        }
    }
}

impl ImportView {
    pub fn start(
        &mut self,
        inbox_dirs: &[PathBuf],
        music_dir: &std::path::Path,
        _conn: &Connection,
        rate_limit_ms: u64,
        name_script: crate::config::settings::NameScriptPreference,
        write_tags: bool,
        auto_match_threshold: f64,
        match_candidates: u32,
        db_path: PathBuf,
    ) {
        self.step = ImportStep::SelectSource;
        self.groups.clear();
        self.current_group = 0;
        self.result_summary = None;
        self.scan_rx = None;
        self.mb_rx = None;
        self.mb_tx = None;
        self.mb_client = None;
        self.mbid_input = None;
        self.mbid_fetch_rx = None;
        self.release_fetch_rx = None;
        self.release_fetch_tx = None;
        self.collection_picker = None;
        self.import_rx = None;
        self.conflicts.clear();
        self.decisions.clear();
        self.conflict_cursor = 0;
        self.confirm_cancel = None;
        self.rate_limit_ms = rate_limit_ms;
        self.name_script = name_script;
        self.write_tags = write_tags;
        self.auto_match_threshold = auto_match_threshold;
        self.match_candidates = match_candidates;
        self.db_path = db_path;

        // Reset SelectSource fields
        self.custom_path = TextInput::new("~/Music/new-album").with_label(" Path: ");
        self.use_custom_path = false;
        self.custom_path_error = None;

        // Collect source paths: inbox dirs first, then music_dir (marked as
        // "library" in the UI). Including music_dir every scan is what makes
        // the wizard notice files that ended up in the library outside the
        // normal import flow — e.g. manual drops, old imports, or leftovers
        // from a cleanup script. Skip music_dir if it's already listed as an
        // inbox so we don't double-scan identical paths.
        self.source_paths.clear();
        self.library_source_index = None;
        for dir in inbox_dirs {
            if dir.exists() {
                self.source_paths.push(dir.clone());
            }
        }
        if music_dir.exists() && !self.source_paths.iter().any(|p| p == music_dir) {
            self.library_source_index = Some(self.source_paths.len());
            self.source_paths.push(music_dir.to_path_buf());
        }
    }

    /// True when the wizard has an active popup (cancel confirmation,
    /// MBID input, collection picker) that should capture key input and
    /// suppress global shortcuts like `q`, `g`, `/`.
    pub fn has_popup(&self) -> bool {
        self.confirm_cancel.is_some()
            || self.mbid_input.is_some()
            || self.collection_picker.is_some()
    }

    /// True when the wizard is actively capturing text input and global
    /// key shortcuts (q, g, /) should be suppressed.
    pub fn is_capturing_input(&self) -> bool {
        (self.step == ImportStep::SelectSource && self.use_custom_path)
            || self.mbid_input.is_some()
            || self.collection_picker.is_some()
    }

    pub fn can_cancel(&self) -> bool {
        matches!(
            self.step,
            ImportStep::SelectSource
                | ImportStep::Review
                | ImportStep::ResolveDuplicates
                | ImportStep::Complete
        )
    }

    pub fn is_complete(&self) -> bool {
        self.step == ImportStep::Complete
    }
}
