//! Key-handling and async-kickoff logic for the import wizard.
//!
//! Owns the state transitions (`handle_key`, its sub-handlers, group
//! navigation) and fires off the background threads for scanning, MB
//! searching, MBID fetching, and importing. The receivers live in
//! `ImportView`; results are drained in `render::tick`.

use std::sync::{Arc, Mutex, mpsc};

use crossterm::event::{KeyCode, KeyEvent};
use rusqlite::Connection;

use crate::config::paths::expand_tilde;
use crate::core::{importer, tagger};
use crate::external::matching;
use crate::external::musicbrainz::MbClient;
use crate::tui::keybindings as keys;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::pick_collection::{PickAction, PickCollectionPopup};

use super::worker::run_import_worker;
use super::{
    GroupAction, ImportGroup, ImportStep, ImportView, MbCandidate, MbMatchState, MbResult,
    ScanMessage,
};

impl ImportView {
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
                if let Some(group) = self.groups.get_mut(self.current_group)
                    && idx < group.mb_candidates.len() {
                        group.selected_candidate = Some(idx);
                        group.action = GroupAction::AcceptMb;
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
                if let Some(group) = self.groups.get_mut(self.current_group)
                    && !group.mb_candidates.is_empty() {
                        let current = group.selected_candidate.unwrap_or(0);
                        if current > 0 {
                            group.selected_candidate = Some(current - 1);
                            group.action = GroupAction::AcceptMb;
                        }
                    }
            }
            KeyCode::Down => {
                if let Some(group) = self.groups.get_mut(self.current_group)
                    && !group.mb_candidates.is_empty() {
                        let current = group.selected_candidate.unwrap_or(0);
                        let max = group.mb_candidates.len() - 1;
                        if current < max {
                            group.selected_candidate = Some(current + 1);
                            group.action = GroupAction::AcceptMb;
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
            KeyCode::Char('r') => self.retry_mb_for_current_group(),
            _ => {}
        }
    }

    /// User-initiated retry of the MB search for the current group. Only
    /// does anything when the group is currently in the `Failed` state —
    /// resets it to `NotStarted` and re-kicks the search.
    fn retry_mb_for_current_group(&mut self) {
        let idx = self.current_group;
        let should_retry = matches!(
            self.groups.get(idx).map(|g| &g.mb_state),
            Some(MbMatchState::Failed(_))
        );
        if !should_retry {
            return;
        }
        if let Some(group) = self.groups.get_mut(idx) {
            group.mb_state = MbMatchState::NotStarted;
            group.mb_candidates.clear();
        }
        self.search_mb_for_group(idx);
    }

    /// Fetch a release by MBID on a background thread.
    fn fetch_mbid(&mut self, mbid: String) {
        let idx = self.current_group;

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

        self.ensure_mb_infra();
        let client = self.mb_client.as_ref().unwrap().clone();

        std::thread::spawn(move || {
            let mut client = client.lock().unwrap();
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
                        error: None,
                    });
                }
                Err(e) => {
                    tracing::warn!("MB fetch_release({}) failed: {}", mbid, e);
                    let _ = tx.send(MbResult {
                        group_idx: idx,
                        candidates: Vec::new(),
                        error: Some(short_mb_error(&e.to_string())),
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
            self.ensure_mb_searches_around_cursor();
        }
    }

    fn prev_group(&mut self) {
        if self.current_group > 0 {
            self.current_group -= 1;
            self.ensure_mb_searches_around_cursor();
        }
    }

    /// Kick off MB searches for the current group and the next three. Each
    /// call is idempotent — `search_mb_for_group` short-circuits groups
    /// that are already `Searching`/`Done`/`Failed`, so navigating back
    /// and forth doesn't duplicate work. Throttle still serializes requests,
    /// so wall-clock is unchanged; this just queues enough work that the user
    /// can navigate several groups forward before hitting a throbber.
    fn ensure_mb_searches_around_cursor(&mut self) {
        let cur = self.current_group;
        for offset in 0..=3 {
            self.search_mb_for_group(cur + offset);
        }
    }

    pub(super) fn is_in_summary(&self) -> bool {
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
            let mut groups: std::collections::HashMap<String, Vec<(crate::db::models::Track, Option<tagger::TagData>)>> =
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
    /// Does nothing if the group is already searched, searching, or failed —
    /// state stays the same on repeated calls, which makes prefetching safe.
    pub(super) fn search_mb_for_group(&mut self, idx: usize) {
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

        self.ensure_mb_infra();
        // Clones for the worker thread.
        let tx = self.mb_tx.as_ref().unwrap().clone();
        let client = self.mb_client.as_ref().unwrap().clone();

        std::thread::spawn(move || {
            if artist.is_empty() && album.is_empty() {
                let _ = tx.send(MbResult {
                    group_idx: idx,
                    candidates: Vec::new(),
                    error: None,
                });
                return;
            }

            // All MB I/O for this thread happens under the shared lock so
            // the client's throttler serializes requests across prefetches.
            let mut client = client.lock().unwrap();
            let mut search_error: Option<String> = None;
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
                Err(e) => {
                    tracing::warn!(
                        "MB search_releases(artist={:?}, album={:?}) failed: {}",
                        artist,
                        album,
                        e
                    );
                    search_error = Some(short_mb_error(&e.to_string()));
                    Vec::new()
                }
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
            //
            // Skip this expensive refine when the leader is an obvious winner:
            // either high absolute score (>= 0.85 matches the auto-accept
            // threshold) or a large gap to #2. Saves ~1.1s per refetch.
            let leader_score = candidates.first().map(|c| c.score.total).unwrap_or(0.0);
            let runner_up = candidates.get(1).map(|c| c.score.total).unwrap_or(0.0);
            let leader_is_obvious =
                leader_score >= 0.85 || (leader_score - runner_up) >= 0.15;

            if !leader_is_obvious {
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
            }

            let _ = tx.send(MbResult {
                group_idx: idx,
                candidates,
                error: search_error,
            });
        });
    }

    /// Lazily create the MB channel and shared client on first use.
    fn ensure_mb_infra(&mut self) {
        if self.mb_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            self.mb_tx = Some(tx);
            self.mb_rx = Some(rx);
        }
        if self.mb_client.is_none() {
            self.mb_client = Some(Arc::new(Mutex::new(MbClient::new(self.rate_limit_ms))));
        }
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

/// Squash a full error chain down to a short one-liner for the wizard UI.
/// Keeps the first line and trims to ~80 chars so it fits the status row.
fn short_mb_error(msg: &str) -> String {
    let first = msg.lines().next().unwrap_or(msg).trim();
    if first.chars().count() > 80 {
        let truncated: String = first.chars().take(77).collect();
        format!("{}…", truncated)
    } else {
        first.to_string()
    }
}
