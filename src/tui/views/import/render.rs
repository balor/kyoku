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

use super::{GroupAction, ImportMessage, ImportStep, ImportView, MbMatchState, ScanMessage};

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
            // Trigger lazy MB search for the first group, plus prefetch
            // the next three so the user can navigate several groups
            // forward before hitting a throbber.
            for i in 0..=3 {
                self.search_mb_for_group(i);
            }
        }

        // Drain any MB results that completed since the last tick. With
        // prefetch enabled, more than one group can deliver per tick, so
        // we loop until the channel is empty. The channel itself stays
        // open for the whole review session.
        if let Some(rx) = &self.mb_rx {
            while let Ok(result) = rx.try_recv() {
                if let Some(group) = self.groups.get_mut(result.group_idx) {
                    if let Some(err) = result.error {
                        // Preserve any candidates we might have collected; set
                        // Failed so the UI can tell the user to look at the log.
                        group.mb_candidates = result.candidates;
                        group.mb_state = MbMatchState::Failed(err);
                    } else {
                        // Auto-select top candidate if score is high enough
                        if let Some(best) = result.candidates.first()
                            && best.score.total >= 0.85 {
                                group.selected_candidate = Some(0);
                                group.action = GroupAction::AcceptMb;
                            }
                        group.mb_candidates = result.candidates;
                        group.mb_state = MbMatchState::Done;
                    }
                }
            }
        }

        // Process manual MBID fetch result
        let mut fetch_done = false;
        if let Some(rx) = &self.mbid_fetch_rx
            && let Ok(result) = rx.try_recv() {
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
                        group.mb_state = MbMatchState::Done;
                    }
                }
                fetch_done = true;
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
                    let cur_failed = self
                        .groups
                        .get(self.current_group)
                        .map(|g| matches!(g.mb_state, MbMatchState::Failed(_)))
                        .unwrap_or(false);
                    let mut hints = vec![
                        ("↑↓/1-5", "pick MB"),
                        ("m", "MBID"),
                        ("c", "+coll"),
                        ("A", "as-is"),
                        ("S", "skip"),
                        ("L", "loose"),
                        ("Enter/n/p", "nav"),
                        ("Esc", "cancel"),
                    ];
                    if cur_failed {
                        hints.insert(0, ("r", "retry MB"));
                    }
                    hints
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
            const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let frame_idx = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() / 80)
                .unwrap_or(0) as usize)
                % FRAMES.len();
            let p = Paragraph::new(Span::styled(
                format!(" {} Searching MusicBrainz...", FRAMES[frame_idx]),
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
        } else if let MbMatchState::Failed(reason) = &group.mb_state {
            let p = Paragraph::new(Span::styled(
                format!(" MB search failed: {} — press r to retry (log has detail)", reason),
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
