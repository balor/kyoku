//! Tick-loop (draining background channels) and the render tree for the
//! import wizard. All `render_*` methods for the step-by-step screens live
//! here alongside `status_hints` which drives the bottom status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use rusqlite::Connection;

use crate::tui::themes::Theme;

use super::dup_detect::{ConflictDecision, DupOther, DupSignal};
use super::{
    GroupAction, ImportGroup, ImportMessage, ImportStep, ImportView, MbMatchState, ScanMessage,
};

fn throbber_frame() -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let frame_idx = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() / 80)
        .unwrap_or(0) as usize)
        % FRAMES.len();
    FRAMES[frame_idx]
}

fn common_tag_release_mbid(group: &ImportGroup) -> Option<String> {
    let mut ids = group.tracks.iter().map(|(_, td)| {
        td.as_ref()
            .and_then(|td| td.mb_release_id.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });
    let first = ids.next().flatten()?.to_string();
    ids.all(|id| id == Some(first.as_str())).then_some(first)
}

impl ImportView {
    pub fn tick(&mut self, conn: &Connection) {
        use std::sync::mpsc::TryRecvError;

        // Process scan messages. `Disconnected` without a prior `Complete`
        // means the scan thread died (panicked) — it owns the only sender.
        // Treating it like `Empty` would leave the wizard stuck on the
        // Scanning screen forever, with q/Ctrl+C suppressed and no way out
        // short of killing the process.
        let mut scan_done = false;
        let mut scan_failed = false;
        let mut scan_dead = false;
        if let Some(rx) = &self.scan_rx {
            loop {
                match rx.try_recv() {
                    Ok(ScanMessage::Progress(done, total)) => {
                        self.scan_progress = (done, total);
                    }
                    Ok(ScanMessage::Complete {
                        groups,
                        skipped_non_utf8,
                    }) => {
                        self.groups = groups;
                        self.current_group = 0;
                        if skipped_non_utf8 > 0 {
                            self.result_summary = Some(format!(
                                "{} file(s) skipped (non-UTF-8 filename — rename to import)",
                                skipped_non_utf8
                            ));
                        }
                        scan_done = true;
                    }
                    Ok(ScanMessage::Failed(reason)) => {
                        self.result_summary = Some(format!("Scan failed: {reason}"));
                        self.step = ImportStep::Complete;
                        scan_done = true;
                        scan_failed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        scan_dead = !scan_done;
                        break;
                    }
                }
            }
        }
        if scan_done {
            self.scan_rx = None;
            if !scan_failed {
                self.step = ImportStep::Review;
                // Trigger lazy MB search for the first group, plus prefetch
                // the next three so the user can navigate several groups
                // forward before hitting a throbber.
                for i in 0..=3 {
                    self.search_mb_for_group(i);
                }
            }
        } else if scan_dead {
            self.scan_rx = None;
            self.result_summary = Some(
                "Scan failed: the scan worker stopped unexpectedly. \
                 Nothing was imported — check the log and try again."
                    .to_string(),
            );
            self.step = ImportStep::Complete;
        }

        // Drain any MB results that completed since the last tick. With
        // prefetch enabled, more than one group can deliver per tick, so
        // we loop until the channel is empty. The channel itself stays
        // open for the whole review session.
        //
        // Auto-selected groups need a release-fetch kicked off so dup
        // detection has recording MBIDs — collect them here and fire
        // after the borrow ends.
        let mut auto_selected: Vec<usize> = Vec::new();
        let auto_threshold = self.auto_match_threshold;
        if let Some(rx) = &self.mb_rx {
            while let Ok(result) = rx.try_recv() {
                if let Some(group) = self.groups.get_mut(result.group_idx) {
                    if let Some(err) = result.error {
                        // Preserve any candidates we might have collected; set
                        // Failed so the UI can tell the user to look at the log.
                        group.mb_candidates = result.candidates;
                        group.mb_state = MbMatchState::Failed(err);
                    } else {
                        // Auto-select top candidate if score is high enough.
                        // Skip when the user has already made a decision for
                        // this group — otherwise a late-arriving MB result
                        // would clobber their Skip/Loose/explicit pick.
                        if !group.user_decided
                            && let Some(best) = result.candidates.first()
                            && best.score.total >= auto_threshold
                        {
                            group.selected_candidate = Some(0);
                            group.action = GroupAction::AcceptMb;
                            auto_selected.push(result.group_idx);
                        }
                        group.mb_candidates = result.candidates;
                        group.mb_state = MbMatchState::Done;
                    }
                }
            }
        }
        for idx in auto_selected {
            self.ensure_full_release_for_group(idx);
        }

        // Drain release-fetch results triggered for dup detection. Match
        // back to the candidate by MBID so a user-initiated candidate
        // change while the fetch was in flight doesn't smash an unrelated
        // candidate. Preserve api_score (the full-release API returns 100
        // because it's not a search hit).
        let mut release_fetch_completed = false;
        if let Some(rx) = &self.release_fetch_rx {
            while let Ok(msg) = rx.try_recv() {
                release_fetch_completed = true;
                if let Some(group) = self.groups.get_mut(msg.group_idx) {
                    group.full_release_fetching = false;
                    if let Some(full) = msg.release
                        && let Some(cand) = group
                            .mb_candidates
                            .iter_mut()
                            .find(|c| c.release.id == msg.release_mbid)
                    {
                        let preserved_api = cand.release.api_score;
                        cand.release = full;
                        cand.release.api_score = preserved_api;
                    }
                }
            }
        }
        // If release fetches finished and the user is already parked on the
        // summary, re-run detection so the MBID pass can see any new MBIDs
        // (or so non-MBID duplicate checks still run after a fetch failure). The
        // step check is load-bearing: `is_in_summary()` stays true during
        // ResolveDuplicates (entering the resolver doesn't change
        // current_group), and rebuilding the preview there would wipe the
        // user's keep/replace decisions mid-flight and snap the cursor
        // back to the first conflict.
        if release_fetch_completed
            && self.step == ImportStep::Review
            && self.is_in_summary()
            && self.pending_full_mb_fetch_count() == 0
        {
            self.refresh_conflict_preview(conn);
        }

        // Process manual MBID fetch result
        let mut fetch_done = false;
        if let Some(rx) = &self.mbid_fetch_rx
            && let Ok(result) = rx.try_recv()
        {
            if let Some(group) = self.groups.get_mut(result.group_idx) {
                if let Some(err) = result.error {
                    group.mb_state = MbMatchState::Failed(err);
                } else if !result.candidates.is_empty() {
                    // Insert the fetched release at the top of candidates
                    let mut new_candidates = result.candidates;
                    new_candidates.append(&mut group.mb_candidates);
                    group.mb_candidates = new_candidates;
                    group.selected_candidate = Some(0);
                    group.action = GroupAction::AcceptMb;
                    group.user_decided = true;
                    group.mb_state = MbMatchState::Done;
                }
            }
            fetch_done = true;
        }
        if fetch_done {
            self.mbid_fetch_rx = None;
        }

        // Process import worker messages. Same dead-thread handling as the
        // scan channel above — the worker owns the only sender.
        let mut import_done = false;
        let mut import_dead = false;
        if let Some(rx) = &self.import_rx {
            loop {
                match rx.try_recv() {
                    Ok(ImportMessage::Progress(d, t)) => {
                        self.import_progress = (d, t);
                    }
                    Ok(ImportMessage::Complete(summary)) => {
                        self.result_summary = Some(summary);
                        self.step = ImportStep::Complete;
                        import_done = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        import_dead = !import_done;
                        break;
                    }
                }
            }
        }
        if import_done || import_dead {
            self.import_rx = None;
        }
        if import_dead {
            self.result_summary = Some(
                "Import stopped unexpectedly — it may have been partially \
                 applied. Check the log, then re-run the scan to see the \
                 current state."
                    .to_string(),
            );
            self.step = ImportStep::Complete;
        }
    }

    pub fn status_hints(&self) -> Vec<(&str, &str)> {
        match self.step {
            ImportStep::SelectSource => {
                if self.use_custom_path {
                    vec![
                        ("Enter", "scan path"),
                        ("Tab", "use inbox"),
                        ("Esc/q", "cancel"),
                    ]
                } else {
                    vec![
                        ("Enter", "scan inbox"),
                        ("Tab", "enter path"),
                        ("Esc/q", "cancel"),
                    ]
                }
            }
            ImportStep::Scanning => vec![],
            ImportStep::Review => {
                if self.groups.is_empty() {
                    vec![("Esc/q", "back")]
                } else if self.is_in_summary() {
                    if self.pending_full_mb_fetch_count() > 0 {
                        vec![("wait", "MB data"), ("p", "back"), ("Esc/q", "cancel")]
                    } else if self.groups.iter().all(|g| g.action == GroupAction::Skip) {
                        vec![("Enter", "close"), ("p", "back"), ("Esc/q", "cancel")]
                    } else {
                        vec![("Enter", "import"), ("p", "back"), ("Esc/q", "cancel")]
                    }
                } else {
                    let cur_failed = self
                        .groups
                        .get(self.current_group)
                        .map(|g| matches!(g.mb_state, MbMatchState::Failed(_)))
                        .unwrap_or(false);
                    let pick_hint = match self.match_candidates.clamp(1, 9) {
                        1 => "↑↓/1",
                        2 => "↑↓/1-2",
                        3 => "↑↓/1-3",
                        4 => "↑↓/1-4",
                        5 => "↑↓/1-5",
                        6 => "↑↓/1-6",
                        7 => "↑↓/1-7",
                        8 => "↑↓/1-8",
                        _ => "↑↓/1-9",
                    };
                    let mut hints = vec![
                        (pick_hint, "pick MB"),
                        ("m", "MBID"),
                        ("c", "+coll"),
                        ("A", "as-is"),
                        ("S", "skip"),
                        ("L", "loose"),
                        ("F", "skip rest"),
                        ("Enter/n/p", "nav"),
                        ("Esc/q", "cancel"),
                    ];
                    if cur_failed {
                        hints.insert(0, ("r", "retry MB"));
                    }
                    hints
                }
            }
            ImportStep::ResolveDuplicates => vec![
                ("1", "keep A"),
                ("2", "keep B"),
                ("n/p", "nav"),
                ("Enter", "confirm & import"),
                ("Esc/q", "cancel"),
            ],
            ImportStep::Importing => vec![],
            ImportStep::Complete => vec![("any key", "done")],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        match self.step {
            ImportStep::SelectSource => self.render_select_source(frame, area, theme),
            ImportStep::Scanning => self.render_scanning(frame, area, theme),
            ImportStep::Review => self.render_review(frame, area, theme),
            ImportStep::ResolveDuplicates => self.render_resolve_dups(frame, area, theme),
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
            Style::default()
                .fg(theme.fg_dim)
                .add_modifier(Modifier::BOLD)
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
            // The library entry gets an inline "(library)" tag in accent color
            // so the user knows it's not a regular inbox dir — we scan it to
            // catch files the DB doesn't know about (manual drops, leftovers).
            for (idx, path) in self.source_paths.iter().enumerate() {
                let is_library = self.library_source_index == Some(idx);
                let path_span = Span::styled(
                    format!("  {}", path.display()),
                    Style::default().fg(inbox_body_color),
                );
                if is_library {
                    let tag_style = if self.use_custom_path {
                        Style::default().fg(theme.fg_muted)
                    } else {
                        Style::default().fg(theme.accent)
                    };
                    inbox_lines.push(Line::from(vec![
                        path_span,
                        Span::styled("  (library)", tag_style),
                    ]));
                } else {
                    inbox_lines.push(Line::from(path_span));
                }
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
                Constraint::Length(3),         // group header + spacer
                Constraint::Min(3),            // tracks
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
            GroupAction::Loose => Span::styled("[Import loose]", Style::default().fg(theme.yellow)),
        };
        let source_line = Line::from(vec![
            Span::styled(
                format!(" Group {}/{}: ", self.current_group + 1, self.groups.len()),
                Style::default().fg(theme.fg_dim),
            ),
            Span::styled(group_source_display(group), Style::default().fg(theme.fg)),
        ]);

        let albums = group_album_tags(group);
        let mut meta_spans = vec![Span::styled(" Album tag: ", Style::default().fg(theme.fg_muted))];
        match albums.as_slice() {
            [] => meta_spans.push(Span::styled(
                "(none)",
                Style::default()
                    .fg(theme.fg_muted)
                    .add_modifier(Modifier::ITALIC),
            )),
            [album] => meta_spans.push(Span::styled(
                album.clone(),
                Style::default()
                    .fg(theme.accent_alt)
                    .add_modifier(Modifier::BOLD),
            )),
            many => meta_spans.push(Span::styled(
                format!("mixed album tags ({})", many.len()),
                Style::default()
                    .fg(theme.yellow)
                    .add_modifier(Modifier::ITALIC),
            )),
        }
        meta_spans.push(Span::styled(" · ", Style::default().fg(theme.fg_muted)));
        meta_spans.push(Span::styled(
            format!("{} tracks ", group.tracks.len()),
            Style::default().fg(theme.fg_dim),
        ));
        meta_spans.push(action_label);
        // Collection suffix only shows when the group will actually import —
        // otherwise it's contradictory (e.g. "[Skip] → coll: X" misleads the
        // user into thinking the group is still being routed somewhere).
        if !group.target_collection.is_empty() && group.action != GroupAction::Skip {
            meta_spans.push(Span::styled(" · collection: ", Style::default().fg(theme.fg_muted)));
            meta_spans.push(Span::styled(
                group.target_collection.clone(),
                Style::default()
                    .fg(theme.accent_alt)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let p = Paragraph::new(vec![source_line, Line::from(meta_spans), Line::from("")]);
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
                Span::styled(format!("  {} ", track.title), Style::default().fg(theme.fg)),
                Span::styled(format!("— {} ", artist), Style::default().fg(theme.fg_dim)),
                Span::styled(format!("[{}]", album), Style::default().fg(theme.fg_muted)),
            ]));
        }
        let p = Paragraph::new(lines);
        frame.render_widget(p, chunks[1]);

        // MB candidates
        if group.mb_state == MbMatchState::Searching {
            let p = Paragraph::new(Span::styled(
                format!(" {} Searching MusicBrainz...", throbber_frame()),
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
            let tagged_release_mbid = common_tag_release_mbid(group);
            for (i, candidate) in group.mb_candidates.iter().enumerate() {
                let is_selected = group.selected_candidate == Some(i);
                let marker = if is_selected { "▶" } else { " " };
                let score_pct = (candidate.score.total * 100.0) as u8;
                let year_str = candidate
                    .release
                    .year
                    .map(|y| format!(" ({})", y))
                    .unwrap_or_default();
                let country = candidate.release.country.as_deref().unwrap_or("");
                let status_label = if candidate.release.is_pseudo_release() {
                    " pseudo-release"
                } else {
                    ""
                };
                let tag_label = if tagged_release_mbid.as_deref()
                    == Some(candidate.release.id.as_str())
                {
                    " tagged-MBID"
                } else {
                    ""
                };

                let style = if is_selected {
                    Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
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

                // Tiny visual cue so the user doesn't have to eyeball
                // the local track count at the top of the screen against
                // each candidate's count at the bottom: `✓` (green) when
                // the candidate's track total matches the group's, blank
                // otherwise. Single character keeps the row uncluttered.
                let count_match = candidate.release.track_count as usize == group.tracks.len();
                let count_marker = if count_match { " ✓" } else { "" };
                let count_marker_style = if count_match {
                    Style::default()
                        .fg(theme.green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                mb_lines.push(Line::from(vec![
                    Span::styled(format!(" {} {} ", marker, i + 1), style),
                    Span::styled(
                        format!("{}% ", score_pct),
                        Style::default()
                            .fg(score_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} — {}{}",
                            candidate.release.artist, candidate.release.title, year_str
                        ),
                        style,
                    ),
                    Span::styled(
                        format!(" {} {} trk", country, candidate.release.track_count),
                        Style::default().fg(theme.fg_muted),
                    ),
                    Span::styled(count_marker, count_marker_style),
                    Span::styled(status_label, Style::default().fg(theme.yellow)),
                    Span::styled(tag_label, Style::default().fg(theme.accent_alt)),
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
        } else if let MbMatchState::Failed(reason) = &group.mb_state {
            let p = Paragraph::new(Span::styled(
                format!(
                    " MB search failed: {} — press r to retry (log has detail)",
                    reason
                ),
                Style::default().fg(theme.red),
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
        let pending_mb_fetches = self.pending_full_mb_fetch_count();

        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                " All groups reviewed",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
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

        // Disclaimer: imports with MB matches will rewrite the audio files'
        // tag frames (title, artist, album, track number, MBIDs, etc.) so
        // the on-disk tags stay in sync with what we commit to the DB.
        // Shown only when it actually applies — at least one MB group AND
        // write_tags enabled — so it never pops up as dead noise.
        if accept_mb > 0 && self.write_tags {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  Note: ",
                    Style::default()
                        .fg(theme.yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "MB metadata will be written to the tags of {} file(s).",
                        accept_mb_tracks
                    ),
                    Style::default().fg(theme.fg_dim),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                "  Disable in config with [tagging] write_tags = false.",
                Style::default().fg(theme.fg_muted),
            )));
        }

        if pending_mb_fetches > 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} Waiting: ", throbber_frame()),
                    Style::default()
                        .fg(theme.yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "fetching full MusicBrainz data for {} group(s)…",
                        pending_mb_fetches
                    ),
                    Style::default().fg(theme.fg_dim),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                "  Duplicate detection and import confirmation are blocked until this finishes.",
                Style::default().fg(theme.fg_muted),
            )));
        }

        // Duplicate preview — detection runs when the user enters this
        // summary; the count is stashed on the view. Surfacing it here
        // so nobody is surprised by a mid-flow resolver screen.
        if pending_mb_fetches == 0 && !self.conflicts.is_empty() {
            let (lib_count, batch_count) =
                self.conflicts
                    .iter()
                    .fold((0u32, 0u32), |acc, c| match c.other {
                        super::dup_detect::DupOther::Library(_) => (acc.0 + 1, acc.1),
                        super::dup_detect::DupOther::Batch(_) => (acc.0, acc.1 + 1),
                    });
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  Duplicates: ",
                    Style::default()
                        .fg(theme.yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "{} conflict(s) detected ({} already in library, {} within this batch).",
                        self.conflicts.len(),
                        lib_count,
                        batch_count
                    ),
                    Style::default().fg(theme.fg_dim),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                "  You'll pick a side for each before import starts.",
                Style::default().fg(theme.fg_muted),
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            if all_skipped {
                "  Nothing to import. Press Enter or Esc to close, or p to go back and change."
            } else if pending_mb_fetches > 0 {
                "  Please wait for MusicBrainz data to finish fetching. Enter is disabled."
            } else if !self.conflicts.is_empty() {
                "  Press Enter to resolve duplicates, p to go back and change, Esc to cancel."
            } else {
                "  Press Enter to import, p to go back and change, Esc to cancel."
            },
            Style::default().fg(theme.fg_dim),
        )));

        let p = Paragraph::new(lines);
        frame.render_widget(p, area);
    }

    fn render_resolve_dups(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Resolve Duplicates ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(conflict) = self.conflicts.get(self.conflict_cursor) else {
            // Cursor out of range (shouldn't happen — the wizard guards
            // against an empty list). Render a terse hint and let Enter
            // fall through to start_import.
            let p = Paragraph::new(Span::styled(
                "  No conflicts — press Enter to continue.",
                Style::default().fg(theme.fg_dim),
            ));
            frame.render_widget(p, inner);
            return;
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Min(0),    // side-by-side panels
                Constraint::Length(4), // footer hint
            ])
            .split(inner);

        // ── Header: progress + conflict description
        let decision = self.decisions.get(self.conflict_cursor).copied();
        let decision_label = match decision {
            Some(ConflictDecision::KeepOther) => Span::styled(
                "  → keep A",
                Style::default()
                    .fg(theme.green)
                    .add_modifier(Modifier::BOLD),
            ),
            Some(ConflictDecision::KeepNew) => Span::styled(
                "  → keep B (replace)",
                Style::default()
                    .fg(theme.yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            None => Span::raw(""),
        };
        let total = self.conflicts.len();
        let signal_text = match conflict.signal {
            DupSignal::AlbumSlot => "  Same slot on the same album.",
            DupSignal::Mbid => "  Same MusicBrainz recording.",
            DupSignal::AlbumTitle => "  Same album + same title (disc/pos disagree).",
        };
        let header_lines = vec![
            Line::from(Span::styled(
                format!("  Conflict {} of {}", self.conflict_cursor + 1, total),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled(signal_text, Style::default().fg(theme.fg_dim)),
                decision_label,
            ]),
        ];
        frame.render_widget(Paragraph::new(header_lines), chunks[0]);

        // ── Panels: A (other) | B (new)
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);

        let a_label = match &conflict.other {
            DupOther::Library(_) => "A — already in library",
            DupOther::Batch(_) => "A — earlier in this batch",
        };
        let a_lines: Vec<Line> = match &conflict.other {
            DupOther::Library(e) => vec![
                Line::from(Span::styled(
                    format!("  {}", e.title),
                    Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!(
                        "  {} · pos {}",
                        e.artist.as_deref().unwrap_or("—"),
                        e.track_number
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "—".into())
                    ),
                    Style::default().fg(theme.fg_dim),
                )),
                Line::from(Span::styled(
                    format!(
                        "  {} · {} kbps{}",
                        e.file_format.to_uppercase(),
                        e.bitrate
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| "—".into()),
                        e.file_size
                            .map(|s| format!(" · {:.1} MB", s as f64 / 1_000_000.0))
                            .unwrap_or_default(),
                    ),
                    Style::default().fg(theme.fg_muted),
                )),
                Line::from(Span::styled(
                    format!("  {}", e.file_path),
                    Style::default().fg(theme.fg_muted),
                )),
                Line::from(Span::styled(
                    format!("  status: {}", e.tag_status),
                    Style::default().fg(theme.fg_muted),
                )),
            ],
            DupOther::Batch(r) => match self
                .groups
                .get(r.group)
                .and_then(|g| g.tracks.get(r.index))
                .map(|(t, td)| (t.clone(), td.clone()))
            {
                Some((t, _td)) => vec![
                    Line::from(Span::styled(
                        format!("  {}", t.title),
                        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        format!(
                            "  {} · pos {}",
                            t.artist.as_deref().unwrap_or("—"),
                            t.track_number
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".into())
                        ),
                        Style::default().fg(theme.fg_dim),
                    )),
                    Line::from(Span::styled(
                        format!(
                            "  {} · {} kbps",
                            t.file_format.as_str().to_uppercase(),
                            t.bitrate
                                .map(|b| b.to_string())
                                .unwrap_or_else(|| "—".into()),
                        ),
                        Style::default().fg(theme.fg_muted),
                    )),
                    Line::from(Span::styled(
                        format!("  {}", t.file_path.display()),
                        Style::default().fg(theme.fg_muted),
                    )),
                ],
                None => vec![Line::from(Span::styled(
                    "  Batch track is no longer available.",
                    Style::default().fg(theme.fg_muted),
                ))],
            },
        };
        let a_block = Block::default()
            .title(Span::styled(
                format!(" {} ", a_label),
                Style::default().fg(theme.cyan),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));
        frame.render_widget(Paragraph::new(a_lines).block(a_block), panels[0]);

        // B panel — the new batch track
        let Some((new_track, _)) = self
            .groups
            .get(conflict.new.group)
            .and_then(|g| g.tracks.get(conflict.new.index))
            .map(|(t, td)| (t.clone(), td.clone()))
        else {
            let b_block = Block::default()
                .title(Span::styled(
                    " B — new (import) ",
                    Style::default().fg(theme.yellow),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border));
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    "  New batch track is no longer available.",
                    Style::default().fg(theme.fg_muted),
                ))])
                .block(b_block),
                panels[1],
            );
            return;
        };
        let b_lines = vec![
            Line::from(Span::styled(
                format!("  {}", new_track.title),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(
                    "  {} · pos {}",
                    new_track.artist.as_deref().unwrap_or("—"),
                    new_track
                        .track_number
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "—".into())
                ),
                Style::default().fg(theme.fg_dim),
            )),
            Line::from(Span::styled(
                format!(
                    "  {} · {} kbps",
                    new_track.file_format.as_str().to_uppercase(),
                    new_track
                        .bitrate
                        .map(|b| b.to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
                Style::default().fg(theme.fg_muted),
            )),
            Line::from(Span::styled(
                format!("  {}", new_track.file_path.display()),
                Style::default().fg(theme.fg_muted),
            )),
        ];
        let b_block = Block::default()
            .title(Span::styled(
                " B — new (import) ",
                Style::default().fg(theme.yellow),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));
        frame.render_widget(Paragraph::new(b_lines).block(b_block), panels[1]);

        // ── Footer: keys + progress note
        let resolved = self.conflict_cursor + 1;
        let footer_lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  [1] keep A   [2] keep B (replace)   [n/p] nav   [Enter] confirm & import",
                Style::default().fg(theme.fg_dim),
            )),
            Line::from(Span::styled(
                format!(
                    "  Viewing {}/{}. Enter commits all decisions and starts the import.",
                    resolved, total
                ),
                Style::default().fg(theme.fg_muted),
            )),
        ];
        frame.render_widget(Paragraph::new(footer_lines), chunks[2]);
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

        let summary = self.result_summary.as_deref().unwrap_or("No results.");

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

fn group_source_display(group: &super::ImportGroup) -> String {
    group
        .tracks
        .first()
        .and_then(|(track, _)| track.source_dir.as_deref().or_else(|| track.file_path.parent()))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| {
            group
                .name
                .split(" — mixed album tags")
                .next()
                .unwrap_or(&group.name)
                .to_string()
        })
}

fn group_album_tags(group: &super::ImportGroup) -> Vec<String> {
    let mut albums: Vec<String> = group
        .tracks
        .iter()
        .filter_map(|(_, tag)| tag.as_ref()?.album.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    albums.sort();
    albums.dedup();
    albums
}
