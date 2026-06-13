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
    ReleaseFetchResult, ScanMessage,
};

fn display_group_name(
    source_dir: &str,
    tracks: &[(crate::db::models::Track, Option<tagger::TagData>)],
) -> String {
    let mut albums: Vec<String> = tracks
        .iter()
        .filter_map(|(_, tag)| tag.as_ref()?.album.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    albums.sort();
    albums.dedup();

    match albums.len() {
        0 => source_dir.to_string(),
        1 => format!("{source_dir} — {}", albums[0]),
        n => format!("{source_dir} — mixed album tags ({n})"),
    }
}

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
            ImportStep::ResolveDuplicates => {
                self.handle_resolve_dup_key(key);
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
                        // Assigning a collection to a Skip group is
                        // contradictory — flip back to an import action.
                        // Prefer AcceptMb if a candidate is already
                        // selected, else fall back to AcceptAsIs.
                        if group.action == GroupAction::Skip {
                            group.action = if group.selected_candidate.is_some() {
                                GroupAction::AcceptMb
                            } else {
                                GroupAction::AcceptAsIs
                            };
                            group.user_decided = true;
                        }
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
                let mbid = raw.rsplit('/').next().unwrap_or(&raw).trim().to_string();
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
                KeyCode::Char('p') => {
                    // Actions may change after going back — the cached
                    // conflict preview is no longer trustworthy.
                    self.conflicts.clear();
                    self.decisions.clear();
                    self.conflict_cursor = 0;
                    self.prev_group();
                }
                KeyCode::Enter => {
                    if self.groups.iter().all(|g| g.action == GroupAction::Skip) {
                        // Nothing to import — emit an empty completion so the
                        // app can close the wizard on the next Enter.
                        self.result_summary =
                            Some("Nothing imported (all groups skipped)".to_string());
                        self.step = ImportStep::Complete;
                    } else if !self.conflicts.is_empty() {
                        // Detection already ran when we entered the summary
                        // — just jump to the resolver.
                        self.step = ImportStep::ResolveDuplicates;
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
                    group.user_decided = true;
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
                    group.user_decided = true;
                }
                self.next_group();
            }
            KeyCode::Char('L') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::Loose;
                    group.selected_candidate = None;
                    group.user_decided = true;
                }
                self.next_group();
            }
            // Number keys 1-9 select an MB candidate
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as u8 - b'1') as usize;
                if let Some(group) = self.groups.get_mut(self.current_group)
                    && idx < group.mb_candidates.len()
                {
                    group.selected_candidate = Some(idx);
                    group.action = GroupAction::AcceptMb;
                    group.user_decided = true;
                }
                self.ensure_full_release_for_group(self.current_group);
            }
            // 0 deselects MB candidate (back to as-is)
            KeyCode::Char('0') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.selected_candidate = None;
                    group.action = GroupAction::AcceptAsIs;
                    group.user_decided = true;
                }
            }
            // Up/down arrows cycle through MB candidates
            KeyCode::Up => {
                if let Some(group) = self.groups.get_mut(self.current_group)
                    && !group.mb_candidates.is_empty()
                {
                    let current = group.selected_candidate.unwrap_or(0);
                    if current > 0 {
                        group.selected_candidate = Some(current - 1);
                        group.action = GroupAction::AcceptMb;
                        group.user_decided = true;
                    }
                }
                self.ensure_full_release_for_group(self.current_group);
            }
            KeyCode::Down => {
                if let Some(group) = self.groups.get_mut(self.current_group)
                    && !group.mb_candidates.is_empty()
                {
                    let current = group.selected_candidate.unwrap_or(0);
                    let max = group.mb_candidates.len() - 1;
                    if current < max {
                        group.selected_candidate = Some(current + 1);
                        group.action = GroupAction::AcceptMb;
                        group.user_decided = true;
                    }
                }
                self.ensure_full_release_for_group(self.current_group);
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
                let mut input =
                    TextInput::new("Paste release MBID or URL...").with_label(" MBID: ");
                input.focused = true;
                self.mbid_input = Some(input);
            }
            KeyCode::Char('n') => self.next_group(),
            KeyCode::Char('p') => self.prev_group(),
            KeyCode::Char('r') => self.retry_mb_for_current_group(),
            // Bail out of a long review: mark every remaining group as
            // Skip and jump to the summary. Decisions already made on
            // earlier groups are preserved. From the summary the user
            // can `p` back if they change their mind.
            KeyCode::Char('F') => {
                for idx in self.current_group..self.groups.len() {
                    if let Some(g) = self.groups.get_mut(idx) {
                        g.action = GroupAction::Skip;
                        g.selected_candidate = None;
                        g.user_decided = true;
                    }
                }
                self.current_group = self.groups.len();
            }
            _ => {}
        }

        // If that action bumped us into the summary view for the first
        // time, run duplicate detection now so the summary can advertise
        // the count. Cached in `self.conflicts`; cleared when the user
        // hits `p` to go back.
        if self.is_in_summary() && self.conflicts.is_empty() {
            self.refresh_conflict_preview(conn);
        }
    }

    /// Re-run duplicate detection against the current set of groups +
    /// actions. Populates `conflicts` / `decisions` for the summary
    /// line and the resolver step. Called when entering the summary.
    pub(super) fn refresh_conflict_preview(&mut self, conn: &Connection) {
        match super::dup_detect::detect(conn, &self.music_dir, &self.groups) {
            Ok(conflicts) => {
                self.decisions = conflicts.iter().map(default_decision_for).collect();
                self.conflicts = conflicts;
                self.conflict_cursor = 0;
            }
            Err(e) => {
                tracing::warn!("duplicate detection failed: {}", e);
                self.conflicts.clear();
                self.decisions.clear();
                self.conflict_cursor = 0;
            }
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
        let (artist, album, year, track_count, titles, total_ms) = if let Some(group) =
            self.groups.get(idx)
        {
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
            let titles: Vec<String> = group.tracks.iter().map(|(t, _)| t.title.clone()).collect();
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
            // Recover a poisoned lock: a panic in one MB thread must not
            // cascade into panics in every later search thread. The client
            // holds no state worth protecting beyond the throttle timestamp.
            let mut client = client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let unimported =
            importer::scan_inbox(conn, &self.music_dir, &self.source_paths).unwrap_or_default();

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
            let mut groups: std::collections::HashMap<
                String,
                Vec<(crate::db::models::Track, Option<tagger::TagData>)>,
            > = std::collections::HashMap::new();
            let mut group_order: Vec<String> = Vec::new();

            for (i, file_path) in all_files.iter().enumerate() {
                let _ = tx.send(ScanMessage::Progress(i + 1, total));

                let abs_path =
                    std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone());

                if let Ok((mut track, tag_data)) = tagger::read_track_with_tags(&abs_path) {
                    track.file_path = abs_path;

                    // Group by source directory. Mixed-album directories stay a
                    // single group by design; the display name below annotates
                    // them so the review screen is explicit about that choice.
                    let group_key = track
                        .source_dir
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Unknown".to_string());

                    if !groups.contains_key(&group_key) {
                        group_order.push(group_key.clone());
                    }
                    groups
                        .entry(group_key)
                        .or_default()
                        .push((track, Some(tag_data)));
                }
            }

            let import_groups: Vec<ImportGroup> = group_order
                .into_iter()
                .filter_map(|name| {
                    groups.remove(&name).map(|tracks| ImportGroup {
                        name: display_group_name(&name, &tracks),
                        tracks,
                        action: GroupAction::AcceptAsIs,
                        mb_candidates: Vec::new(),
                        selected_candidate: None,
                        mb_state: MbMatchState::NotStarted,
                        target_collection: String::new(),
                        full_release_fetching: false,
                        user_decided: false,
                    })
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
        let limit = self.match_candidates;

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
            // Poison recovery: see fetch-by-MBID thread above.
            let mut client = client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut search_error: Option<String> = None;
            let mut candidates: Vec<MbCandidate> =
                match client.search_releases(&artist, &album, track_count, limit) {
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
            let leader_is_obvious = leader_score >= 0.85 || (leader_score - runner_up) >= 0.15;

            if !leader_is_obvious {
                let tied_count = candidates
                    .iter()
                    .take_while(|c| (leader_score - c.score.total).abs() < 0.10)
                    .count()
                    .min(3); // never refetch more than top 3

                if tied_count > 1 {
                    for i in 0..tied_count {
                        let mbid = candidates[i].release.id.clone();
                        match client.fetch_release(&mbid) {
                            Ok(mut full) => {
                                // Preserve the search-result API score before rescoring:
                                // full release fetch returns api_score = 100 because it is
                                // not a search hit, and using that flat score biases ties.
                                let preserved_api = candidates[i].release.api_score;
                                full.api_score = preserved_api;
                                let new_score = matching::score_release(
                                    &artist,
                                    &album,
                                    year,
                                    track_count,
                                    &titles,
                                    total_ms,
                                    &full,
                                );
                                candidates[i] = MbCandidate {
                                    release: full,
                                    score: new_score,
                                };
                            }
                            Err(e) => {
                                tracing::warn!("tied-leader refine fetch {} failed: {}", mbid, e)
                            }
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
            self.mb_client = Some(Arc::new(Mutex::new(MbClient::new(
                self.rate_limit_ms,
                self.name_script,
            ))));
        }
    }

    /// Lazily create the release-fetch channel on first use.
    fn ensure_release_fetch_channel(&mut self) {
        if self.release_fetch_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            self.release_fetch_tx = Some(tx);
            self.release_fetch_rx = Some(rx);
        }
    }

    /// If the group at `idx` is AcceptMb with a selected candidate whose
    /// `release.tracks` is empty (search results don't include tracks),
    /// spin up a background `fetch_release` so duplicate detection has
    /// the recording MBIDs to key on. Idempotent: short-circuits if the
    /// tracks are already populated or a fetch is already in flight.
    pub(super) fn ensure_full_release_for_group(&mut self, idx: usize) {
        let mbid = {
            let Some(group) = self.groups.get(idx) else {
                return;
            };
            if group.action != GroupAction::AcceptMb {
                return;
            }
            if group.full_release_fetching {
                return;
            }
            let Some(cand_idx) = group.selected_candidate else {
                return;
            };
            let Some(cand) = group.mb_candidates.get(cand_idx) else {
                return;
            };
            if !cand.release.tracks.is_empty() {
                return;
            }
            cand.release.id.clone()
        };
        if mbid.is_empty() {
            return;
        }

        self.ensure_mb_infra();
        self.ensure_release_fetch_channel();

        // Mark in-flight so repeated calls don't pile up parallel fetches
        // for the same group. Cleared in the release_fetch_rx drain.
        if let Some(group) = self.groups.get_mut(idx) {
            group.full_release_fetching = true;
        }

        let client = self.mb_client.as_ref().unwrap().clone();
        let tx = self.release_fetch_tx.as_ref().unwrap().clone();
        let mbid_for_thread = mbid.clone();

        std::thread::spawn(move || {
            let release = {
                // Poison recovery: see fetch-by-MBID thread above.
                let mut client = client
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match client.fetch_release(&mbid_for_thread) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!(
                            "MB fetch_release({}) for dup-detection failed: {}",
                            mbid_for_thread,
                            e
                        );
                        None
                    }
                }
            };
            let _ = tx.send(ReleaseFetchResult {
                group_idx: idx,
                release_mbid: mbid_for_thread,
                release,
            });
        });
    }

    /// Keyboard for the `ResolveDuplicates` step: 1/2 pick a side,
    /// n/p navigate, Enter commit and proceed to import.
    fn handle_resolve_dup_key(&mut self, key: KeyEvent) {
        use super::dup_detect::ConflictDecision as D;
        if self.conflicts.is_empty() {
            // Shouldn't happen, but guard against accidentally stranding
            // the user on an empty picker.
            self.start_import();
            return;
        }
        match key.code {
            KeyCode::Char('1') => {
                if let Some(d) = self.decisions.get_mut(self.conflict_cursor) {
                    *d = D::KeepOther;
                }
                self.advance_conflict_cursor();
            }
            KeyCode::Char('2') => {
                if let Some(d) = self.decisions.get_mut(self.conflict_cursor) {
                    *d = D::KeepNew;
                }
                self.advance_conflict_cursor();
            }
            KeyCode::Char('n') => self.advance_conflict_cursor(),
            KeyCode::Char('p') => {
                if self.conflict_cursor > 0 {
                    self.conflict_cursor -= 1;
                }
            }
            KeyCode::Enter => {
                // All conflicts have a decision already (defaulted on
                // entry, then possibly edited). Fire the import.
                self.start_import();
            }
            _ => {}
        }
    }

    fn advance_conflict_cursor(&mut self) {
        if self.conflict_cursor + 1 < self.conflicts.len() {
            self.conflict_cursor += 1;
        }
    }

    fn start_import(&mut self) {
        self.step = ImportStep::Importing;

        // Materialise the decisions into per-track plans *before* we
        // filter groups, so the (group_idx, track_idx) coordinates in
        // `BatchTrackRef` still align. Then filter Skip groups (plans
        // stay aligned because we map groups+plans in lockstep).
        let plans =
            super::dup_detect::plan_from_decisions(&self.groups, &self.conflicts, &self.decisions);

        let (groups_to_import, plans_to_import): (Vec<ImportGroup>, Vec<Vec<_>>) = self
            .groups
            .iter()
            .zip(plans.into_iter())
            .filter(|(g, _)| g.action != GroupAction::Skip)
            .map(|(g, p)| (g.clone(), p))
            .unzip();

        // Count tracks in groups the user marked Skip — we want to show
        // those in the summary so the user sees what they decided to drop.
        let user_skipped: u32 = self
            .groups
            .iter()
            .filter(|g| g.action == GroupAction::Skip)
            .map(|g| g.tracks.len() as u32)
            .sum();

        // Denominator must match what the worker uses — it excludes tracks
        // the duplicate resolver has already marked as skip. Without this
        // subtraction, the UI briefly shows the pre-skip total until the
        // worker's first Progress message lands.
        let planned_skip: usize = plans_to_import
            .iter()
            .map(|gp| gp.iter().filter(|p| p.skip).count())
            .sum();
        let total_tracks: usize = groups_to_import
            .iter()
            .map(|g| g.tracks.len())
            .sum::<usize>()
            .saturating_sub(planned_skip);
        self.import_progress = (0, total_tracks);

        let rate_limit_ms = self.rate_limit_ms;
        let name_script = self.name_script;
        let write_tags = self.write_tags;
        let db_path = self.db_path.clone();
        let music_dir = self.music_dir.clone();
        let (tx, rx) = mpsc::channel();
        self.import_rx = Some(rx);

        std::thread::spawn(move || {
            run_import_worker(
                groups_to_import,
                plans_to_import,
                user_skipped,
                rate_limit_ms,
                name_script,
                write_tags,
                db_path,
                music_dir,
                tx,
            );
        });
    }
}

/// Pick the conservative default for a freshly-detected conflict. Library
/// conflicts default to "keep what's already there" (no destructive op);
/// intra-batch conflicts default to "keep the first" (the later one is
/// dropped). User can override with 1/2/S before confirming.
fn default_decision_for(
    conflict: &super::dup_detect::Conflict,
) -> super::dup_detect::ConflictDecision {
    use super::dup_detect::{ConflictDecision, DupOther};
    match conflict.other {
        DupOther::Library(_) => ConflictDecision::KeepOther,
        DupOther::Batch(_) => ConflictDecision::KeepOther,
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
