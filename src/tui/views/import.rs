//! Top-level state for the import wizard. The detailed handling lives in
//! sibling submodules — `wizard` owns key/async logic, `render` owns tick
//! and every draw call, `worker` hosts the standalone import thread.

mod render;
mod wizard;
mod worker;

use std::path::PathBuf;
use std::sync::mpsc;

use rusqlite::Connection;

use crate::core::tagger;
use crate::db::models::Track;
use crate::external::matching::MatchScore;
use crate::external::musicbrainz::MbRelease;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::pick_collection::PickCollectionPopup;

#[derive(Debug, Clone, PartialEq)]
pub enum ImportStep {
    SelectSource,
    Scanning,
    Review,
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
}

#[derive(Clone, Copy, PartialEq)]
pub enum GroupAction {
    AcceptAsIs,
    AcceptMb,
    Skip,
    Loose,
}

pub struct ImportView {
    pub step: ImportStep,
    pub source_paths: Vec<PathBuf>,
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
    scan_rx: Option<mpsc::Receiver<ScanMessage>>,
    /// Receives results for the currently-searched group.
    mb_rx: Option<mpsc::Receiver<MbResult>>,
    /// Manual MBID input mode during Review.
    mbid_input: Option<TextInput>,
    /// Receives a fetched release from a manual MBID lookup.
    mbid_fetch_rx: Option<mpsc::Receiver<MbResult>>,
    /// Per-group collection picker (during Review). When `Some`, captures input.
    collection_picker: Option<PickCollectionPopup>,
    /// Receives progress messages from the background importer.
    import_rx: Option<mpsc::Receiver<ImportMessage>>,
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

impl Default for ImportView {
    fn default() -> Self {
        Self {
            step: ImportStep::SelectSource,
            source_paths: Vec::new(),
            groups: Vec::new(),
            current_group: 0,
            scan_progress: (0, 0),
            import_progress: (0, 0),
            result_summary: None,
            custom_path: TextInput::new("~/Music/new-album").with_label(" Path: "),
            use_custom_path: false,
            custom_path_error: None,
            rate_limit_ms: 1100,
            scan_rx: None,
            mb_rx: None,
            mbid_input: None,
            mbid_fetch_rx: None,
            collection_picker: None,
            import_rx: None,
        }
    }
}

impl ImportView {
    pub fn start(&mut self, inbox_dirs: &[PathBuf], _conn: &Connection, rate_limit_ms: u64) {
        self.step = ImportStep::SelectSource;
        self.groups.clear();
        self.current_group = 0;
        self.result_summary = None;
        self.scan_rx = None;
        self.mb_rx = None;
        self.mbid_input = None;
        self.mbid_fetch_rx = None;
        self.collection_picker = None;
        self.import_rx = None;
        self.rate_limit_ms = rate_limit_ms;

        // Reset SelectSource fields
        self.custom_path = TextInput::new("~/Music/new-album").with_label(" Path: ");
        self.use_custom_path = false;
        self.custom_path_error = None;

        // Collect source paths from inbox
        self.source_paths.clear();
        for dir in inbox_dirs {
            if dir.exists() {
                self.source_paths.push(dir.clone());
            }
        }
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
            ImportStep::SelectSource | ImportStep::Review | ImportStep::Complete
        )
    }

    pub fn is_complete(&self) -> bool {
        self.step == ImportStep::Complete
    }
}
