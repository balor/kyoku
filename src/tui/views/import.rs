use std::path::PathBuf;
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use rusqlite::Connection;

use crate::core::importer;
use crate::core::tagger;
use crate::db::models::Track;
use crate::db::queries;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;

#[derive(Debug, Clone, PartialEq)]
pub enum ImportStep {
    SelectSource,
    Scanning,
    Review,
    Importing,
    Complete,
}

#[derive(Clone)]
pub struct ImportGroup {
    pub name: String,
    pub tracks: Vec<(Track, Option<tagger::TagData>)>,
    pub action: GroupAction,
}

#[derive(Clone, Copy, PartialEq)]
pub enum GroupAction {
    AcceptAsIs,
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
    scan_rx: Option<mpsc::Receiver<ScanMessage>>,
}

enum ScanMessage {
    Progress(usize, usize),
    Complete(Vec<ImportGroup>),
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
            scan_rx: None,
        }
    }
}

impl ImportView {
    pub fn start(&mut self, inbox_dirs: &[PathBuf], _conn: &Connection) {
        self.step = ImportStep::SelectSource;
        self.groups.clear();
        self.current_group = 0;
        self.result_summary = None;
        self.scan_rx = None;

        // Collect source paths from inbox
        self.source_paths.clear();
        for dir in inbox_dirs {
            if dir.exists() {
                self.source_paths.push(dir.clone());
            }
        }
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
                if keys::is_confirm(&key) && !self.source_paths.is_empty() {
                    self.start_scan(conn);
                }
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

    fn handle_review_key(&mut self, key: KeyEvent, conn: &Connection) {
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
                        self.start_import(conn);
                    }
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('A') | KeyCode::Enter => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::AcceptAsIs;
                }
                self.next_group();
            }
            KeyCode::Char('S') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::Skip;
                }
                self.next_group();
            }
            KeyCode::Char('L') => {
                if let Some(group) = self.groups.get_mut(self.current_group) {
                    group.action = GroupAction::Loose;
                }
                self.next_group();
            }
            KeyCode::Char('n') => self.next_group(),
            KeyCode::Char('p') => self.prev_group(),
            _ => {}
        }
    }

    /// Advance cursor by one group. When called on the last group, advances
    /// *past* the end into the review-summary state (current_group == len()).
    fn next_group(&mut self) {
        if self.current_group < self.groups.len() {
            self.current_group += 1;
        }
    }

    fn prev_group(&mut self) {
        if self.current_group > 0 {
            self.current_group -= 1;
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
                })
                .collect();

            let _ = tx.send(ScanMessage::Complete(import_groups));
        });
    }

    fn start_import(&mut self, conn: &Connection) {
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

        let mut imported = 0u32;
        let mut skipped = 0u32;
        let mut errors = 0u32;

        for group in &groups_to_import {
            let loose = group.action == GroupAction::Loose;

            for (track, tag_data) in &group.tracks {
                let path_str = track.file_path.display().to_string();

                // Check if already exists
                if queries::track_exists_by_path(conn, &path_str).unwrap_or(false) {
                    skipped += 1;
                    self.import_progress.0 += 1;
                    continue;
                }

                // Get or create album if not loose
                let album_id = if !loose {
                    if let Some(td) = tag_data {
                        if let Some(album_title) = &td.album {
                            queries::get_or_create_album(
                                conn,
                                album_title,
                                td.album_artist.as_deref(),
                                td.year.map(|y| y as i32),
                                td.genre.as_deref(),
                                group.tracks.len() as u32,
                            )
                            .ok()
                            .map(|(id, _)| id)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let file_size = std::fs::metadata(&track.file_path)
                    .map(|m| m.len() as i64)
                    .ok();

                match queries::insert_track(conn, track, album_id, file_size) {
                    Ok(_) => imported += 1,
                    Err(_) => errors += 1,
                }
                self.import_progress.0 += 1;
            }
        }

        let mut parts = vec![format!("Imported: {}", imported)];
        if skipped > 0 {
            parts.push(format!("Duplicates: {}", skipped));
        }
        if user_skipped > 0 {
            parts.push(format!("Skipped: {}", user_skipped));
        }
        if errors > 0 {
            parts.push(format!("Errors: {}", errors));
        }
        self.result_summary = Some(parts.join(", "));
        self.step = ImportStep::Complete;
    }

    pub fn tick(&mut self, _conn: &Connection) {
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
                        self.step = ImportStep::Review;
                        scan_done = true;
                    }
                }
            }
        }
        if scan_done {
            self.scan_rx = None;
        }
    }

    pub fn status_hints(&self) -> Vec<(&str, &str)> {
        match self.step {
            ImportStep::SelectSource => vec![("Enter", "start scan"), ("Esc", "cancel")],
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
                        ("Enter/A", "as-is"),
                        ("S", "skip"),
                        ("L", "loose"),
                        ("n/p", "nav groups"),
                        ("Esc", "cancel"),
                    ]
                }
            }
            ImportStep::Importing => vec![],
            ImportStep::Complete => vec![("Enter/Esc", "done")],
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

        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "Sources to scan:",
                Style::default()
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];

        if self.source_paths.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No inbox directories configured.",
                Style::default().fg(theme.fg_muted),
            )));
            lines.push(Line::from(Span::styled(
                "  Add inbox_dirs to your config.",
                Style::default().fg(theme.fg_muted),
            )));
        } else {
            for path in &self.source_paths {
                lines.push(Line::from(Span::styled(
                    format!("  {}", path.display()),
                    Style::default().fg(theme.fg),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Press Enter to start scanning.",
                Style::default().fg(theme.fg_dim),
            )));
        }

        let p = Paragraph::new(lines);
        frame.render_widget(p, inner);
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

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // group nav
                Constraint::Length(1), // separator
                Constraint::Min(5),   // tracks
            ])
            .split(inner);

        // Group navigation
        let group = &self.groups[self.current_group];
        let action_label = match group.action {
            GroupAction::AcceptAsIs => Span::styled("[Accept as-is]", Style::default().fg(theme.green)),
            GroupAction::Skip => Span::styled("[Skip]", Style::default().fg(theme.red)),
            GroupAction::Loose => Span::styled("[Import loose]", Style::default().fg(theme.yellow)),
        };
        let nav = Line::from(vec![
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
        ]);
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
        frame.render_widget(p, chunks[2]);
    }

    fn render_review_summary(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut accept = 0usize;
        let mut loose = 0usize;
        let mut skip = 0usize;
        let mut accept_tracks = 0usize;
        let mut loose_tracks = 0usize;
        let mut skip_tracks = 0usize;
        for g in &self.groups {
            match g.action {
                GroupAction::AcceptAsIs => {
                    accept += 1;
                    accept_tracks += g.tracks.len();
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

        let all_skipped = accept == 0 && loose == 0;

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

        if accept > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} group(s) ", accept),
                    Style::default().fg(theme.fg_dim),
                ),
                Span::styled("accept as-is", Style::default().fg(theme.green)),
                Span::styled(
                    format!(" · {} tracks", accept_tracks),
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
                "Press Enter or Esc to return to library.",
                Style::default().fg(theme.fg_dim),
            )),
        ];
        let p = Paragraph::new(lines);
        frame.render_widget(p, inner);
    }
}
