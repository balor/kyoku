use std::path::PathBuf;
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use rusqlite::Connection;

use crate::config::paths::expand_tilde;
use crate::core::importer;
use crate::core::tagger;
use crate::db::models::Track;
use crate::db::queries;
use crate::external::matching::{self, MatchScore};
use crate::external::musicbrainz::{MbRelease, MbClient};
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::pick_collection::{PickAction, PickCollectionPopup};

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

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) {
        match self.step {
            ImportStep::SelectSource => {
                self.handle_select_source_key(key, conn);
            }
            ImportStep::Scanning => {
                // Can't interact during scan
            }
            ImportStep::Review => {
                self.handle_review_key(key, conn);
            }
            ImportStep::Importing => {
                // Can't interact during import
            }
            ImportStep::Complete => {
                // Esc/Enter handled by app
            }
        }
    }

    fn handle_select_source_key(&mut self, key: KeyEvent, conn: &Connection) {
        // Tab toggles focus between "scan inbox dirs" and "enter custom path".
        if key.code == KeyCode::Tab {
            self.use_custom_path = !self.use_custom_path;
            self.custom_path.focused = self.use_custom_path;
            self.custom_path_error = None;
            return;
        }

        if self.use_custom_path {
            if keys::is_back(&key) {
                if !self.custom_path.value.is_empty() {
                    self.custom_path.clear();
                    self.custom_path_error = None;
                    return;
                }
                self.use_custom_path = false;
                self.custom_path.focused = false;
                return;
            }

            if keys::is_confirm(&key) {
                let raw = self.custom_path.value.trim();
                if raw.is_empty() {
                    self.custom_path_error = Some("Enter a directory path".to_string());
                    return;
                }
                let expanded = expand_tilde(raw);
                if !expanded.exists() {
                    self.custom_path_error =
                        Some(format!("Path does not exist: {}", expanded.display()));
                    return;
                }
                if !expanded.is_dir() {
                    self.custom_path_error =
                        Some(format!("Not a directory: {}", expanded.display()));
                    return;
                }
                self.custom_path_error = None;
                self.source_paths = vec![expanded];
                self.start_scan(conn);
                return;
            }

            if self.custom_path.handle_key(key) {
                self.custom_path_error = None;
            }
            return;
        }

        if keys::is_confirm(&key) && !self.source_paths.is_empty() {
            self.start_scan(conn);
        }
    }

    fn handle_review_key(&mut self, key: KeyEvent, conn: &Connection) {
        // Per-group collection picker captures all keys
        if let Some(picker) = &mut self.collection_picker {
            match picker.handle_key(key) {
                PickAction::None => return,
                PickAction::Cancel => {
                    self.collection_picker = None;
                    return;
                }
                PickAction::Picked(name) => {
                    if let Some(group) = self.groups.get_mut(self.current_group) {
                        group.target_collection = name.trim().to_string();
                    }
                    self.collection_picker = None;
                    return;
                }
            }
        }

        // Manual MBID input captures all keys
        if let Some(input) = &mut self.mbid_input {
            if keys::is_back(&key) {
                self.mbid_input = None;
                return;
            }
            if keys::is_confirm(&key) {
                let raw = input.value.trim().to_string();
                // Accept a full URL or just the UUID
                let mbid = raw
                    .rsplit('/')
                    .next()
                    .unwrap_or(&raw)
                    .trim()
                    .to_string();
                if !mbid.is_empty() {
                    self.fetch_mbid(mbid);
                }
                self.mbid_input = None;
                return;
            }
            input.handle_key(key);
            return;
        }

        if self.groups.is_empty() {
            return;
        }

        // In summary state: Enter confirms (or closes if nothing to import),
        // p goes back to the last group to change a decision.
        if self.is_in_summary() {
            match key.code {
                KeyCode::Char('p') => self.prev_group(),
                KeyCode::Enter => {
                    if self.groups.iter().all(|g| g.action == GroupAction::Skip) {
                        // Nothing to import — emit an empty completion so the
                        // app can close the wizard on the next Enter.
                        self.result_summary =
                            Some("Nothing imported (all groups skipped)".to_string());
                        self.step = ImportStep::Complete;
                    } else {
                        self.start_import();
                    }
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('A') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::AcceptAsIs;
                    group.selected_candidate = None;
                }
                self.next_group();
            }
            KeyCode::Enter => {
                // Enter just advances to the next group, keeping whatever
                // action is already set (AcceptMb if user picked 1-5,
                // AcceptAsIs by default, etc.)
                self.next_group();
            }
            KeyCode::Char('S') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::Skip;
                    group.selected_candidate = None;
                }
                self.next_group();
            }
            KeyCode::Char('L') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::Loose;
                    group.selected_candidate = None;
                }
                self.next_group();
            }
            // Number keys 1-9 select an MB candidate
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as u8 - b'1') as usize;
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    if idx < group.mb_candidates.len() {
                        group.selected_candidate = Some(idx);
                        group.action = GroupAction::AcceptMb;
                    }
                }
            }
            // 0 deselects MB candidate (back to as-is)
            KeyCode::Char('0') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.selected_candidate = None;
                    group.action = GroupAction::AcceptAsIs;
                }
            }
            // Up/down arrows cycle through MB candidates
            KeyCode::Up => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    if !group.mb_candidates.is_empty() {
                        let current = group.selected_candidate.unwrap_or(0);
                        if current > 0 {
                            group.selected_candidate = Some(current - 1);
                            group.action = GroupAction::AcceptMb;
                        }
                    }
                }
            }
            KeyCode::Down => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    if !group.mb_candidates.is_empty() {
                        let current = group.selected_candidate.unwrap_or(0);
                        let max = group.mb_candidates.len() - 1;
                        if current < max {
                            group.selected_candidate = Some(current + 1);
                            group.action = GroupAction::AcceptMb;
                        }
                    }
                }
            }
            KeyCode::Char('c') => {
                // Open per-group collection picker
                if let Some(group) = self.groups.get(self.current_group) {
                    let current = group.target_collection.clone();
                    // Default name = the last component of the group's source dir
                    let default_name = std::path::Path::new(&group.name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&group.name)
                        .to_string();
                    self.collection_picker = Some(PickCollectionPopup::open(
                        format!("Add to collection — {}", group.name),
                        default_name,
                        &current,
                        conn,
                    ));
                }
            }
            KeyCode::Char('m') => {
                // Open manual MBID input
                let mut input = TextInput::new(
                    "Paste release MBID or URL...",
                )
                .with_label(" MBID: ");
                input.focused = true;
                self.mbid_input = Some(input);
            }
            KeyCode::Char('n') => self.next_group(),
            KeyCode::Char('p') => self.prev_group(),
            _ => {}
        }
    }

    /// Fetch a release by MBID on a background thread.
    fn fetch_mbid(&mut self, mbid: String) {
        let idx = self.current_group;
        let rate_limit_ms = self.rate_limit_ms;

        // Get local data for scoring
        let (artist, album, year, track_count, titles, total_ms) =
            if let Some(group) = self.groups.get(idx) {
                let first_tag = group.tracks.first().and_then(|(_, td)| td.as_ref());
                let artist = first_tag
                    .and_then(|td| td.album_artist.as_deref().or(td.artist.as_deref()))
                    .unwrap_or("")
                    .to_string();
                let album = first_tag
                    .and_then(|td| td.album.as_deref())
                    .unwrap_or(&group.name)
                    .to_string();
                let year = first_tag.and_then(|td| td.year.map(|y| y as i32));
                let tc = group.tracks.len() as u32;
                let titles: Vec<String> =
                    group.tracks.iter().map(|(t, _)| t.title.clone()).collect();
                let ms: u64 = group
                    .tracks
                    .iter()
                    .map(|(t, _)| t.duration_ms.unwrap_or(0))
                    .sum();
                (artist, album, year, tc, titles, ms)
            } else {
                return;
            };

        let (tx, rx) = mpsc::channel();
        self.mbid_fetch_rx = Some(rx);

        std::thread::spawn(move || {
            let mut client = MbClient::new(rate_limit_ms);
            match client.fetch_release(&mbid) {
                Ok(release) => {
                    let score = matching::score_release(
                        &artist,
                        &album,
                        year,
                        track_count,
                        &titles,
                        total_ms,
                        &release,
                    );
                    let _ = tx.send(MbResult {
                        group_idx: idx,
                        candidates: vec![MbCandidate { release, score }],
                    });
                }
                Err(_) => {
                    let _ = tx.send(MbResult {
                        group_idx: idx,
                        candidates: Vec::new(),
                    });
                }
            }
        });
    }

    /// Advance cursor by one group. When called on the last group, advances
    /// *past* the end into the review-summary state (current_group == len()).
    fn next_group(&mut self) {
        if self.current_group < self.groups.len() {
            self.current_group += 1;
            self.search_mb_for_current_group();
        }
    }

    fn prev_group(&mut self) {
        if self.current_group > 0 {
            self.current_group -= 1;
            self.search_mb_for_current_group();
        }
    }

    fn is_in_summary(&self) -> bool {
        !self.groups.is_empty() && self.current_group >= self.groups.len()
    }

    fn start_scan(&mut self, conn: &Connection) {
        self.step = ImportStep::Scanning;

        // Filter out files that are already in the DB (main thread — needs conn).
        // This is the same logic used by `kyoku scan` for the inbox indicator.
        let unimported = importer::scan_inbox(conn, &self.source_paths).unwrap_or_default();

        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);

        // If nothing to import, jump straight to Review (empty) so the user
        // gets feedback instead of a hanging Scanning screen.
        if unimported.is_empty() {
            self.groups.clear();
            self.current_group = 0;
            self.step = ImportStep::Review;
            self.scan_rx = None;
            return;
        }

        std::thread::spawn(move || {
            let all_files = unimported;
            let total = all_files.len();
            let mut groups: std::collections::HashMap<String, Vec<(Track, Option<tagger::TagData>)>> =
                std::collections::HashMap::new();

            for (i, file_path) in all_files.iter().enumerate() {
                let _ = tx.send(ScanMessage::Progress(i + 1, total));

                let abs_path =
                    std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone());

                match tagger::read_track(&abs_path) {
                    Ok(mut track) => {
                        track.file_path = abs_path;
                        let tag_data = tagger::read_tags(file_path).ok();

                        // Group by source directory
                        let group_key = track
                            .source_dir
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "Unknown".to_string());

                        groups
                            .entry(group_key)
                            .or_default()
                            .push((track, tag_data));
                    }
                    Err(_) => {} // skip errors during scan
                }
            }

            let import_groups: Vec<ImportGroup> = groups
                .into_iter()
                .map(|(name, tracks)| ImportGroup {
                    name,
                    tracks,
                    action: GroupAction::AcceptAsIs,
                    mb_candidates: Vec::new(),
                    selected_candidate: None,
                    mb_state: MbMatchState::NotStarted,
                    target_collection: String::new(),
                })
                .collect();

            let _ = tx.send(ScanMessage::Complete(import_groups));
        });
    }

    /// Kick off an MB search for the given group index on a background thread.
    /// Does nothing if the group is already searched or searching.
    fn search_mb_for_current_group(&mut self) {
        let idx = self.current_group;
        let group = match self.groups.get_mut(idx) {
            Some(g) if g.mb_state == MbMatchState::NotStarted => g,
            _ => return,
        };
        group.mb_state = MbMatchState::Searching;

        let first_tag = group.tracks.first().and_then(|(_, td)| td.as_ref());
        let artist = first_tag
            .and_then(|td| td.album_artist.as_deref().or(td.artist.as_deref()))
            .unwrap_or("")
            .to_string();
        let album = first_tag
            .and_then(|td| td.album.as_deref())
            .unwrap_or(&group.name)
            .to_string();
        let year = first_tag.and_then(|td| td.year.map(|y| y as i32));
        let track_count = group.tracks.len() as u32;
        let titles: Vec<String> = group.tracks.iter().map(|(t, _)| t.title.clone()).collect();
        let total_ms: u64 = group
            .tracks
            .iter()
            .map(|(t, _)| t.duration_ms.unwrap_or(0))
            .sum();
        let rate_limit_ms = self.rate_limit_ms;

        let (tx, rx) = mpsc::channel();
        self.mb_rx = Some(rx);

        std::thread::spawn(move || {
            if artist.is_empty() && album.is_empty() {
                let _ = tx.send(MbResult {
                    group_idx: idx,
                    candidates: Vec::new(),
                });
                return;
            }

            let mut client = MbClient::new(rate_limit_ms);
            let mut candidates: Vec<MbCandidate> = match client
                .search_releases(&artist, &album, 5)
            {
                Ok(releases) => releases
                    .into_iter()
                    .map(|r| {
                        let score = matching::score_release(
                            &artist,
                            &album,
                            year,
                            track_count,
                            &titles,
                            total_ms,
                            &r,
                        );
                        MbCandidate { release: r, score }
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };

            // Initial sort by coarse score
            candidates.sort_by(|a, b| {
                b.score
                    .total
                    .partial_cmp(&a.score.total)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            // Tiebreaker: when top candidates are within 10% of the leader,
            // fetch the full release for each tied candidate so we can compare
            // track titles and durations. The search API doesn't return tracks,
            // so this is the only way to differentiate releases by translated
            // titles, alternate editions, etc.
            let leader_score = candidates.first().map(|c| c.score.total).unwrap_or(0.0);
            let tied_count = candidates
                .iter()
                .take_while(|c| (leader_score - c.score.total).abs() < 0.10)
                .count()
                .min(3); // never refetch more than top 3

            if tied_count > 1 {
                for i in 0..tied_count {
                    let mbid = candidates[i].release.id.clone();
                    if let Ok(full) = client.fetch_release(&mbid) {
                        let new_score = matching::score_release(
                            &artist,
                            &album,
                            year,
                            track_count,
                            &titles,
                            total_ms,
                            &full,
                        );
                        // Preserve the search-result API score (full release fetch
                        // returns api_score = 100 because it's not a search hit)
                        let preserved_api = candidates[i].release.api_score;
                        let mut full = full;
                        full.api_score = preserved_api;
                        candidates[i] = MbCandidate {
                            release: full,
                            score: new_score,
                        };
                    }
                }
                // Re-sort after refetching
                candidates.sort_by(|a, b| {
                    b.score
                        .total
                        .partial_cmp(&a.score.total)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            let _ = tx.send(MbResult {
                group_idx: idx,
                candidates,
            });
        });
    }

    fn start_import(&mut self) {
        self.step = ImportStep::Importing;

        let groups_to_import: Vec<ImportGroup> = self
            .groups
            .iter()
            .filter(|g| g.action != GroupAction::Skip)
            .cloned()
            .collect();

        // Count tracks in groups the user marked Skip — we want to show
        // those in the summary so the user sees what they decided to drop.
        let user_skipped: u32 = self
            .groups
            .iter()
            .filter(|g| g.action == GroupAction::Skip)
            .map(|g| g.tracks.len() as u32)
            .sum();

        let total_tracks: usize = groups_to_import.iter().map(|g| g.tracks.len()).sum();
        self.import_progress = (0, total_tracks);

        let rate_limit_ms = self.rate_limit_ms;
        let (tx, rx) = mpsc::channel();
        self.import_rx = Some(rx);

        std::thread::spawn(move || {
            run_import_worker(groups_to_import, user_skipped, rate_limit_ms, tx);
        });
    }
}

/// Background worker for the import phase. Opens its own DB connection so we
/// don't block the TUI's main-thread `Connection`.
fn run_import_worker(
    groups_to_import: Vec<ImportGroup>,
    user_skipped: u32,
    rate_limit_ms: u64,
    tx: mpsc::Sender<ImportMessage>,
) {
    let conn = match crate::db::open_database(crate::config::paths::database_file()) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(ImportMessage::Complete(format!("DB open failed: {}", e)));
            return;
        }
    };
    let conn = &conn;

    let total_tracks: usize = groups_to_import.iter().map(|g| g.tracks.len()).sum();
    let mut done = 0usize;
    let mut imported = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;
    let mut added_to_collection = 0u32;

    // Fetch full release data for MB-matched groups (search results don't
    // include track listings — we need them for per-track metadata).
    let mut mb_client = MbClient::new(rate_limit_ms);

    {
        let _ = tx.send(ImportMessage::Progress(done, total_tracks));
        for group in &groups_to_import {
            let loose = group.action == GroupAction::Loose;

            // Resolve this group's target collection (if any)
            let target_collection_name = group.target_collection.trim();
            let target_collection_id = if !target_collection_name.is_empty() {
                queries::get_or_create_collection(conn, target_collection_name)
                    .ok()
                    .map(|(id, _)| id)
            } else {
                None
            };

            // If MB match, fetch the full release once for the whole group
            let mb_full = if group.action == GroupAction::AcceptMb {
                group
                    .selected_candidate
                    .and_then(|idx| group.mb_candidates.get(idx))
                    .and_then(|c| mb_client.fetch_release(&c.release.id).ok())
            } else {
                None
            };

            // For MB-matched groups, all tracks share one album (the MB release).
            // Precompute it once.
            let mb_album_id = if loose {
                None
            } else if let Some(mb) = &mb_full {
                queries::get_or_create_album(
                    conn,
                    &mb.title,
                    Some(&mb.artist),
                    mb.year,
                    None,
                    group.tracks.len() as u32,
                )
                .ok()
                .map(|(id, _)| id)
            } else {
                None
            };

            // Update MB album with MB metadata
            if let (Some(aid), Some(mb)) = (mb_album_id, &mb_full) {
                queries::update_album_mb(
                    conn,
                    aid,
                    &mb.id,
                    None,
                    &mb.artist,
                    &mb.title,
                    mb.year,
                    mb.label.as_deref(),
                )
                .ok();
            }

            // For "Accept as-is" mode, decide if this group is a real album
            // (all tracks share the same album+album_artist tags) or a compilation.
            // Compilations get NO per-track albums — those tracks become loose
            // and live entirely in their assigned collection (if any).
            //
            // Rules for "real album":
            // 1. All tracks share the same (album, album_artist) tuple
            // 2. The album_artist is NOT a "various" marker
            //    (Various, Various Artists, VA, Compilation, etc.)
            // 3. Track-level artists are not wildly diverse
            //    (compilations often have one unique artist per track)
            let group_is_real_album = !loose && mb_full.is_none() && {
                let key = |td: &Option<tagger::TagData>| -> Option<(String, Option<String>)> {
                    td.as_ref().and_then(|t| {
                        t.album.as_ref().map(|a| {
                            (
                                a.clone(),
                                t.album_artist.clone().or_else(|| t.artist.clone()),
                            )
                        })
                    })
                };
                let first_key = group.tracks.first().and_then(|(_, td)| key(td));
                let consistent_album_key =
                    first_key.is_some() && group.tracks.iter().all(|(_, td)| key(td) == first_key);

                if !consistent_album_key {
                    false
                } else {
                    // Check rule 2: album_artist isn't a "various" marker
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
                        false
                    } else {
                        // Check rule 3: track-level artist diversity
                        // Real albums typically have 1-3 distinct artists (main + features).
                        // If >= 4 distinct artists OR >= 40% of tracks have unique artists,
                        // it's a compilation.
                        use std::collections::HashSet;
                        let unique_artists: HashSet<String> = group
                            .tracks
                            .iter()
                            .filter_map(|(_, td)| {
                                td.as_ref().and_then(|t| t.artist.as_ref()).cloned()
                            })
                            .collect();
                        let n_tracks = group.tracks.len().max(1);
                        let too_diverse = unique_artists.len() >= 4
                            || (unique_artists.len() * 100 / n_tracks) >= 40;
                        !too_diverse
                    }
                }
            };

            // Precompute the as-is album once if we determined this is a real album
            let asis_album_id = if group_is_real_album {
                let first_tag = group.tracks.first().and_then(|(_, td)| td.as_ref());
                first_tag.and_then(|td| {
                    td.album.as_ref().and_then(|album_title| {
                        queries::get_or_create_album(
                            conn,
                            album_title,
                            td.album_artist.as_deref().or(td.artist.as_deref()),
                            td.year.map(|y| y as i32),
                            td.genre.as_deref(),
                            group.tracks.len() as u32,
                        )
                        .ok()
                        .map(|(id, _)| id)
                    })
                })
            } else {
                None
            };

            for (i, (track, _tag_data)) in group.tracks.iter().enumerate() {
                let path_str = track.file_path.display().to_string();

                if queries::track_exists_by_path(conn, &path_str).unwrap_or(false) {
                    skipped += 1;
                    done += 1;
                    let _ = tx.send(ImportMessage::Progress(done, total_tracks));
                    continue;
                }

                // Per-track album resolution:
                // - Loose: no album
                // - MB-matched: use the precomputed MB album (one per group)
                // - Real album (as-is): use the precomputed shared album
                // - Compilation (as-is): no album → tracks become loose, but
                //   they can still be in a collection
                let album_id = if loose {
                    None
                } else if let Some(id) = mb_album_id {
                    Some(id)
                } else if let Some(id) = asis_album_id {
                    Some(id)
                } else {
                    None
                };

                let file_size = std::fs::metadata(&track.file_path)
                    .map(|m| m.len() as i64)
                    .ok();

                match queries::insert_track(conn, track, album_id, file_size) {
                    Ok(track_id) => {
                        imported += 1;

                        // Apply per-track MB data (title, artist, recording MBID)
                        if let Some(mb) = &mb_full {
                            // Match local track to MB track by position (1-based)
                            let mb_track = mb.tracks.iter().find(|t| t.position == (i + 1) as u32);
                            if let Some(mbt) = mb_track {
                                queries::update_track_mb(
                                    conn,
                                    track_id,
                                    &mbt.recording_id,
                                    mbt.artist.as_deref().unwrap_or(&mb.artist),
                                    &mbt.title,
                                    "matched",
                                )
                                .ok();
                            } else {
                                // No positional match — still mark as matched at album level
                                queries::set_track_tag_status(conn, track_id, "matched")
                                    .ok();
                            }
                        }

                        // Add to target collection if user requested one
                        if let Some(coll_id) = target_collection_id {
                            if queries::add_track_to_collection(conn, coll_id, track_id)
                                .unwrap_or(false)
                            {
                                added_to_collection += 1;
                            }
                        }
                    }
                    Err(_) => errors += 1,
                }
                done += 1;
                let _ = tx.send(ImportMessage::Progress(done, total_tracks));
            }
        }
    }

    let mut parts = vec![format!("Imported: {}", imported)];
    if added_to_collection > 0 {
        parts.push(format!("Added to collections: {}", added_to_collection));
    }
    if skipped > 0 {
        parts.push(format!("Duplicates: {}", skipped));
    }
    if user_skipped > 0 {
        parts.push(format!("Skipped: {}", user_skipped));
    }
    if errors > 0 {
        parts.push(format!("Errors: {}", errors));
    }
    let _ = tx.send(ImportMessage::Complete(parts.join(", ")));
}

impl ImportView {
    pub fn tick(&mut self, _conn: &Connection) {
        // Process scan messages
        let mut scan_done = false;
        if let Some(rx) = &self.scan_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    ScanMessage::Progress(done, total) => {
                        self.scan_progress = (done, total);
                    }
                    ScanMessage::Complete(groups) => {
                        self.groups = groups;
                        self.current_group = 0;
                        scan_done = true;
                    }
                }
            }
        }
        if scan_done {
            self.scan_rx = None;
            self.step = ImportStep::Review;
            // Trigger lazy MB search for the first group
            self.search_mb_for_current_group();
        }

        // Process MB result for the currently-searched group
        let mut mb_done = false;
        if let Some(rx) = &self.mb_rx {
            if let Ok(result) = rx.try_recv() {
                if let Some(group) = self.groups.get_mut(result.group_idx) {
                    // Auto-select top candidate if score is high enough
                    if let Some(best) = result.candidates.first() {
                        if best.score.total >= 0.85 {
                            group.selected_candidate = Some(0);
                            group.action = GroupAction::AcceptMb;
                        }
                    }
                    group.mb_candidates = result.candidates;
                    group.mb_state = MbMatchState::Done;
                }
                mb_done = true;
            }
        }
        if mb_done {
            self.mb_rx = None;
        }

        // Process manual MBID fetch result
        let mut fetch_done = false;
        if let Some(rx) = &self.mbid_fetch_rx {
            if let Ok(result) = rx.try_recv() {
                if let Some(group) = self.groups.get_mut(result.group_idx) {
                    if !result.candidates.is_empty() {
                        // Insert the fetched release at the top of candidates
                        let mut new_candidates = result.candidates;
                        new_candidates.extend(group.mb_candidates.drain(..));
                        group.mb_candidates = new_candidates;
                        group.selected_candidate = Some(0);
                        group.action = GroupAction::AcceptMb;
                        group.mb_state = MbMatchState::Done;
                    }
                }
                fetch_done = true;
            }
        }
        if fetch_done {
            self.mbid_fetch_rx = None;
        }

        // Process import worker messages
        let mut import_done = false;
        if let Some(rx) = &self.import_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    ImportMessage::Progress(d, t) => {
                        self.import_progress = (d, t);
                    }
                    ImportMessage::Complete(summary) => {
                        self.result_summary = Some(summary);
                        self.step = ImportStep::Complete;
                        import_done = true;
                    }
                }
            }
        }
        if import_done {
            self.import_rx = None;
        }
    }

    pub fn status_hints(&self) -> Vec<(&str, &str)> {
        match self.step {
            ImportStep::SelectSource => {
                if self.use_custom_path {
                    vec![
                        ("Enter", "scan path"),
                        ("Tab", "use inbox"),
                        ("Esc", "cancel"),
                    ]
                } else {
                    vec![
                        ("Enter", "scan inbox"),
                        ("Tab", "enter path"),
                        ("Esc", "cancel"),
                    ]
                }
            }
            ImportStep::Scanning => vec![],
            ImportStep::Review => {
                if self.groups.is_empty() {
                    vec![("Esc", "back")]
                } else if self.is_in_summary() {
                    if self.groups.iter().all(|g| g.action == GroupAction::Skip) {
                        vec![("Enter", "close"), ("p", "back"), ("Esc", "cancel")]
                    } else {
                        vec![("Enter", "import"), ("p", "back"), ("Esc", "cancel")]
                    }
                } else {
                    vec![
                        ("↑↓/1-5", "pick MB"),
                        ("m", "MBID"),
                        ("c", "+coll"),
                        ("A", "as-is"),
                        ("S", "skip"),
                        ("L", "loose"),
                        ("Enter/n/p", "nav"),
                        ("Esc", "cancel"),
                    ]
                }
            }
            ImportStep::Importing => vec![],
            ImportStep::Complete => vec![("any key", "done")],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        match self.step {
            ImportStep::SelectSource => self.render_select_source(frame, area, theme),
            ImportStep::Scanning => self.render_scanning(frame, area, theme),
            ImportStep::Review => self.render_review(frame, area, theme),
            ImportStep::Importing => self.render_importing(frame, area, theme),
            ImportStep::Complete => self.render_complete(frame, area, theme),
        }
    }

    fn render_select_source(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Import Wizard ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),    // inbox list + heading
                Constraint::Length(2), // custom-path heading + separator
                Constraint::Length(1), // path input
                Constraint::Length(1), // error / hint
            ])
            .split(inner);

        // Inbox section
        let inbox_header_style = if self.use_custom_path {
            Style::default().fg(theme.fg_dim).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        };
        let mut inbox_lines = vec![
            Line::from(""),
            Line::from(Span::styled("Inbox sources:", inbox_header_style)),
            Line::from(""),
        ];

        let inbox_body_color = if self.use_custom_path {
            theme.fg_muted
        } else {
            theme.fg
        };

        if self.source_paths.is_empty() {
            inbox_lines.push(Line::from(Span::styled(
                "  (no inbox directories configured)",
                Style::default().fg(theme.fg_muted),
            )));
        } else {
            for path in &self.source_paths {
                inbox_lines.push(Line::from(Span::styled(
                    format!("  {}", path.display()),
                    Style::default().fg(inbox_body_color),
                )));
            }
        }
        let p = Paragraph::new(inbox_lines);
        frame.render_widget(p, chunks[0]);

        // Custom-path heading
        let custom_heading_style = if self.use_custom_path {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg_dim)
        };
        let heading = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "Or import a specific directory:",
                custom_heading_style,
            )),
        ]);
        frame.render_widget(heading, chunks[1]);

        self.custom_path.render(frame, chunks[2], theme);

        // Bottom line: error or hint
        let (text, style) = if let Some(err) = &self.custom_path_error {
            (err.clone(), Style::default().fg(theme.red))
        } else if self.use_custom_path {
            (
                "  Enter to scan this path · Tab to use inbox".to_string(),
                Style::default().fg(theme.fg_muted),
            )
        } else if self.source_paths.is_empty() {
            (
                "  No inbox — Tab to enter a custom path".to_string(),
                Style::default().fg(theme.fg_muted),
            )
        } else {
            (
                "  Enter to scan inbox · Tab to enter a custom path".to_string(),
                Style::default().fg(theme.fg_muted),
            )
        };
        let p = Paragraph::new(Span::styled(text, style));
        frame.render_widget(p, chunks[3]);
    }

    fn render_scanning(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Scanning... ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (done, total) = self.scan_progress;
        let ratio = if total > 0 {
            done as f64 / total as f64
        } else {
            0.0
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);

        let label = format!("Scanning: {}/{} files", done, total);
        let p = Paragraph::new(Span::styled(label, Style::default().fg(theme.fg)));
        frame.render_widget(p, chunks[0]);

        let gauge = Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(theme.accent).bg(theme.bg_alt));
        frame.render_widget(gauge, chunks[1]);
    }

    fn render_review(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Review Import ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.groups.is_empty() {
            let p = Paragraph::new(Span::styled(
                "No files found to import.",
                Style::default().fg(theme.fg_muted),
            ));
            frame.render_widget(p, inner);
            return;
        }

        // Review summary state: cursor is past the last group
        if self.is_in_summary() {
            self.render_review_summary(frame, inner, theme);
            return;
        }

        let group = &self.groups[self.current_group];
        let has_candidates =
            group.mb_state == MbMatchState::Done && !group.mb_candidates.is_empty();

        let mb_height = if has_candidates {
            (group.mb_candidates.len() as u16 + 2).min(8)
        } else if group.mb_state == MbMatchState::Searching {
            2
        } else {
            1
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // group nav
                Constraint::Min(3),   // tracks
                Constraint::Length(mb_height), // MB candidates
            ])
            .split(inner);

        // Group navigation
        let action_label = match group.action {
            GroupAction::AcceptAsIs => {
                Span::styled("[Accept as-is]", Style::default().fg(theme.green))
            }
            GroupAction::AcceptMb => {
                let idx = group.selected_candidate.unwrap_or(0) + 1;
                Span::styled(
                    format!("[MB match #{}]", idx),
                    Style::default().fg(theme.cyan),
                )
            }
            GroupAction::Skip => Span::styled("[Skip]", Style::default().fg(theme.red)),
            GroupAction::Loose => {
                Span::styled("[Import loose]", Style::default().fg(theme.yellow))
            }
        };
        let mut nav_spans = vec![
            Span::styled(
                format!(
                    " Group {}/{}: {} ({} tracks) ",
                    self.current_group + 1,
                    self.groups.len(),
                    group.name,
                    group.tracks.len(),
                ),
                Style::default().fg(theme.fg),
            ),
            action_label,
        ];
        if !group.target_collection.is_empty() {
            nav_spans.push(Span::styled(
                format!(" → coll: {}", group.target_collection),
                Style::default()
                    .fg(theme.accent_alt)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let nav = Line::from(nav_spans);
        let p = Paragraph::new(nav);
        frame.render_widget(p, chunks[0]);

        // Track list
        let mut lines = Vec::new();
        for (track, tag_data) in &group.tracks {
            let album = tag_data
                .as_ref()
                .and_then(|td| td.album.as_deref())
                .unwrap_or("");
            let artist = track.artist.as_deref().unwrap_or("");
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", track.title),
                    Style::default().fg(theme.fg),
                ),
                Span::styled(
                    format!("— {} ", artist),
                    Style::default().fg(theme.fg_dim),
                ),
                Span::styled(
                    format!("[{}]", album),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
        let p = Paragraph::new(lines);
        frame.render_widget(p, chunks[1]);

        // MB candidates
        if group.mb_state == MbMatchState::Searching {
            let p = Paragraph::new(Span::styled(
                " Searching MusicBrainz...",
                Style::default()
                    .fg(theme.accent_alt)
                    .add_modifier(Modifier::ITALIC),
            ));
            frame.render_widget(p, chunks[2]);
        } else if has_candidates {
            let mut mb_lines = vec![Line::from(Span::styled(
                " MusicBrainz matches:",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))];
            for (i, candidate) in group.mb_candidates.iter().enumerate() {
                let is_selected = group.selected_candidate == Some(i);
                let marker = if is_selected { "▶" } else { " " };
                let score_pct = (candidate.score.total * 100.0) as u8;
                let year_str = candidate
                    .release
                    .year
                    .map(|y| format!(" ({})", y))
                    .unwrap_or_default();
                let country = candidate
                    .release
                    .country
                    .as_deref()
                    .unwrap_or("");

                let style = if is_selected {
                    Style::default()
                        .fg(theme.fg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg_dim)
                };
                let score_color = if score_pct >= 85 {
                    theme.green
                } else if score_pct >= 60 {
                    theme.yellow
                } else {
                    theme.red
                };

                mb_lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} {} ", marker, i + 1),
                        style,
                    ),
                    Span::styled(
                        format!("{}% ", score_pct),
                        Style::default().fg(score_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{} — {}{}", candidate.release.artist, candidate.release.title, year_str),
                        style,
                    ),
                    Span::styled(
                        format!(" {} {} trk", country, candidate.release.track_count),
                        Style::default().fg(theme.fg_muted),
                    ),
                ]));
            }
            let p = Paragraph::new(mb_lines);
            frame.render_widget(p, chunks[2]);
        } else if group.mb_state == MbMatchState::Done {
            let p = Paragraph::new(Span::styled(
                " No MusicBrainz matches found",
                Style::default().fg(theme.fg_muted),
            ));
            frame.render_widget(p, chunks[2]);
        } else if group.mb_state == MbMatchState::NotStarted {
            let p = Paragraph::new(Span::styled(
                " MusicBrainz search pending...",
                Style::default().fg(theme.fg_muted),
            ));
            frame.render_widget(p, chunks[2]);
        }

        // MBID input popup
        if let Some(input) = &self.mbid_input {
            use crate::tui::widgets::popup;
            let content = vec![Line::from("")];
            let popup_inner = popup::render_popup(
                frame,
                area,
                theme,
                "Enter MusicBrainz Release ID",
                &content,
                70,
                5,
            );
            input.render(frame, popup_inner, theme);
        }

        // Per-group collection picker popup
        if let Some(picker) = &self.collection_picker {
            picker.render(frame, area, theme);
        }

        // Show "Fetching..." if MBID lookup is in progress
        if self.mbid_fetch_rx.is_some() {
            use crate::tui::widgets::popup;
            let content = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Fetching release from MusicBrainz...",
                    Style::default().fg(theme.accent_alt),
                )),
            ];
            popup::render_popup(frame, area, theme, "Loading", &content, 50, 5);
        }
    }

    fn render_review_summary(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut accept_asis = 0usize;
        let mut accept_mb = 0usize;
        let mut loose = 0usize;
        let mut skip = 0usize;
        let mut accept_asis_tracks = 0usize;
        let mut accept_mb_tracks = 0usize;
        let mut loose_tracks = 0usize;
        let mut skip_tracks = 0usize;
        for g in &self.groups {
            match g.action {
                GroupAction::AcceptAsIs => {
                    accept_asis += 1;
                    accept_asis_tracks += g.tracks.len();
                }
                GroupAction::AcceptMb => {
                    accept_mb += 1;
                    accept_mb_tracks += g.tracks.len();
                }
                GroupAction::Loose => {
                    loose += 1;
                    loose_tracks += g.tracks.len();
                }
                GroupAction::Skip => {
                    skip += 1;
                    skip_tracks += g.tracks.len();
                }
            }
        }

        let all_skipped = accept_asis == 0 && accept_mb == 0 && loose == 0;

        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                " All groups reviewed",
                Style::default()
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];

        if accept_mb > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} group(s) ", accept_mb),
                    Style::default().fg(theme.fg_dim),
                ),
                Span::styled("MB matched", Style::default().fg(theme.cyan)),
                Span::styled(
                    format!(" · {} tracks", accept_mb_tracks),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
        if accept_asis > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} group(s) ", accept_asis),
                    Style::default().fg(theme.fg_dim),
                ),
                Span::styled("accept as-is", Style::default().fg(theme.green)),
                Span::styled(
                    format!(" · {} tracks", accept_asis_tracks),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
        if loose > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} group(s) ", loose),
                    Style::default().fg(theme.fg_dim),
                ),
                Span::styled("import loose", Style::default().fg(theme.yellow)),
                Span::styled(
                    format!(" · {} tracks", loose_tracks),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
        if skip > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} group(s) ", skip),
                    Style::default().fg(theme.fg_dim),
                ),
                Span::styled("skip", Style::default().fg(theme.red)),
                Span::styled(
                    format!(" · {} tracks", skip_tracks),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            if all_skipped {
                "  Nothing to import. Press Enter or Esc to close, or p to go back and change."
            } else {
                "  Press Enter to import, p to go back and change, Esc to cancel."
            },
            Style::default().fg(theme.fg_dim),
        )));

        let p = Paragraph::new(lines);
        frame.render_widget(p, area);
    }

    fn render_importing(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Importing... ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (done, total) = self.import_progress;
        let ratio = if total > 0 {
            done as f64 / total as f64
        } else {
            0.0
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);

        let label = format!("Importing: {}/{} tracks", done, total);
        let p = Paragraph::new(Span::styled(label, Style::default().fg(theme.fg)));
        frame.render_widget(p, chunks[0]);

        let gauge = Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(theme.green).bg(theme.bg_alt));
        frame.render_widget(gauge, chunks[1]);
    }

    fn render_complete(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Import Complete ",
                Style::default()
                    .fg(theme.green)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let summary = self
            .result_summary
            .as_deref()
            .unwrap_or("No results.");

        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(summary, Style::default().fg(theme.fg))),
            Line::from(""),
            Line::from(Span::styled(
                "Press any key to return to library.",
                Style::default().fg(theme.fg_dim),
            )),
        ];
        let p = Paragraph::new(lines);
        frame.render_widget(p, inner);
    }
}
