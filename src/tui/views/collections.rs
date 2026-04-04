use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::db::queries::{self, CollectionRow, TrackRow};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::views::library::format_duration_ms;
use crate::tui::widgets::input::TextInput;

pub enum CollectionsAction {
    None,
    OpenCollection(i64),
    Refresh,
    SwitchToLibrary,
}

pub enum CollectionDetailAction {
    None,
    EditTrack(i64),
    Refresh,
}

enum InputMode {
    Normal,
    Creating(TextInput),
    Renaming { input: TextInput, id: i64 },
    ConfirmDelete,
}

pub struct CollectionsView {
    pub collections: Vec<CollectionRow>,
    pub selected: usize,
    mode: InputMode,
}

impl Default for CollectionsView {
    fn default() -> Self {
        Self {
            collections: Vec::new(),
            selected: 0,
            mode: InputMode::Normal,
        }
    }
}

impl CollectionsView {
    pub fn load(&mut self, conn: &Connection, search: Option<&str>) -> Result<()> {
        self.collections = if let Some(q) = search {
            if !q.is_empty() {
                queries::search_collections(conn, q)?
            } else {
                queries::list_collections(conn)?
            }
        } else {
            queries::list_collections(conn)?
        };
        if self.collections.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.collections.len() {
            self.selected = self.collections.len() - 1;
        }
        Ok(())
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) -> CollectionsAction {
        match &mut self.mode {
            InputMode::Creating(input) => {
                if keys::is_back(&key) {
                    self.mode = InputMode::Normal;
                    return CollectionsAction::None;
                }
                if keys::is_confirm(&key) {
                    let name = input.value.trim().to_string();
                    if !name.is_empty() {
                        queries::create_collection(conn, &name).ok();
                    }
                    self.mode = InputMode::Normal;
                    return CollectionsAction::Refresh;
                }
                input.handle_key(key);
                return CollectionsAction::None;
            }
            InputMode::Renaming { input, id } => {
                if keys::is_back(&key) {
                    self.mode = InputMode::Normal;
                    return CollectionsAction::None;
                }
                if keys::is_confirm(&key) {
                    let name = input.value.trim().to_string();
                    if !name.is_empty() {
                        queries::rename_collection(conn, *id, &name).ok();
                    }
                    self.mode = InputMode::Normal;
                    return CollectionsAction::Refresh;
                }
                input.handle_key(key);
                return CollectionsAction::None;
            }
            InputMode::ConfirmDelete => {
                if key.code == KeyCode::Char('y') {
                    if let Some(coll) = self.collections.get(self.selected) {
                        queries::delete_collection(conn, coll.id).ok();
                    }
                    self.mode = InputMode::Normal;
                    return CollectionsAction::Refresh;
                }
                self.mode = InputMode::Normal;
                return CollectionsAction::None;
            }
            InputMode::Normal => {}
        }

        let count = self.collections.len();

        if keys::is_up(&key) && self.selected > 0 {
            self.selected -= 1;
        }
        if keys::is_down(&key) && count > 0 && self.selected < count - 1 {
            self.selected += 1;
        }
        if keys::is_page_up(&key) {
            self.selected = self.selected.saturating_sub(20);
        }
        if keys::is_page_down(&key) && count > 0 {
            self.selected = (self.selected + 20).min(count - 1);
        }
        if keys::is_half_page_up(&key) {
            self.selected = self.selected.saturating_sub(10);
        }
        if keys::is_half_page_down(&key) && count > 0 {
            self.selected = (self.selected + 10).min(count - 1);
        }
        if key.code == KeyCode::Char('G') && count > 0 {
            self.selected = count - 1;
        }
        if keys::is_confirm(&key) {
            if let Some(coll) = self.collections.get(self.selected) {
                return CollectionsAction::OpenCollection(coll.id);
            }
        }
        if key.code == KeyCode::Char('n') {
            let mut input = TextInput::new("Collection name...").with_label(" Name: ");
            input.focused = true;
            self.mode = InputMode::Creating(input);
            return CollectionsAction::None;
        }
        if key.code == KeyCode::Char('R') {
            if let Some(coll) = self.collections.get(self.selected) {
                let mut input =
                    TextInput::new("New collection name...").with_label(" Name: ");
                input.value = coll.name.clone();
                input.cursor = input.value.len();
                input.focused = true;
                self.mode = InputMode::Renaming {
                    input,
                    id: coll.id,
                };
            }
            return CollectionsAction::None;
        }
        if key.code == KeyCode::Char('d') && !self.collections.is_empty() {
            self.mode = InputMode::ConfirmDelete;
            return CollectionsAction::None;
        }

        CollectionsAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut rows = Vec::new();
        for (i, coll) in self.collections.iter().enumerate() {
            let is_selected = i == self.selected;
            let desc = coll.description.as_deref().unwrap_or("");

            let row = Row::new(vec![
                Cell::from(coll.name.clone()),
                Cell::from(format!("{:>4}", coll.track_count)),
                Cell::from(desc.to_string()),
            ]);

            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD)
            } else if i % 2 == 0 {
                Style::default().bg(theme.bg).fg(theme.fg)
            } else {
                Style::default().bg(theme.bg_alt).fg(theme.fg)
            };

            rows.push(row.style(style));
        }

        if self.collections.is_empty() {
            let msg = Paragraph::new(Span::styled(
                " No collections yet. Press 'n' to create one.",
                Style::default().fg(theme.fg_muted),
            ))
            .style(Style::default().bg(theme.bg));
            frame.render_widget(msg, area);
            return;
        }

        let header = Row::new(vec![
            Cell::from(Span::styled("Collection", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Tracks", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Description", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(30),
                Constraint::Length(8),
                Constraint::Percentage(50),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(theme.border)),
        );

        frame.render_widget(table, area);

        // Render input overlay if creating
        if let InputMode::Creating(input) = &self.mode {
            use crate::tui::widgets::popup;
            use ratatui::text::Line;
            let content = vec![Line::from("")];
            let inner = popup::render_popup(frame, area, theme, "New Collection", &content, 50, 5);
            input.render(frame, inner, theme);
        }

        // Render input overlay if renaming
        if let InputMode::Renaming { input, .. } = &self.mode {
            use crate::tui::widgets::popup;
            use ratatui::text::Line;
            let content = vec![Line::from("")];
            let inner =
                popup::render_popup(frame, area, theme, "Rename Collection", &content, 50, 5);
            input.render(frame, inner, theme);
        }

        // Render delete confirmation
        if matches!(self.mode, InputMode::ConfirmDelete) {
            use crate::tui::widgets::popup;
            use ratatui::text::Line;
            let name = self
                .collections
                .get(self.selected)
                .map(|c| c.name.as_str())
                .unwrap_or("?");
            let content = vec![
                Line::from(format!("Delete collection '{}'?", name)),
                Line::from(""),
                Line::from(Span::styled(
                    "y = yes, any other key = cancel",
                    Style::default().fg(theme.fg_muted),
                )),
            ];
            popup::render_popup(frame, area, theme, "Confirm Delete", &content, 50, 7);
        }
    }

    pub fn render_detail_bar(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if let Some(coll) = self.collections.get(self.selected) {
            let duration = format_duration_ms(coll.total_duration_ms);
            let detail = format!(" {} · {} tracks · {}", coll.name, coll.track_count, duration);
            let p = Paragraph::new(Span::styled(detail, Style::default().fg(theme.fg_dim)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
        }
    }
}

// ── Collection Detail View ──────────────────────────────────────────

pub struct CollectionDetailView {
    pub collection: Option<CollectionRow>,
    pub tracks: Vec<TrackRow>,
    pub filter: String,
    pub selected: usize,
    pub scroll_offset: usize,
    confirm_remove: bool,
    rename_input: Option<TextInput>,
}

impl Default for CollectionDetailView {
    fn default() -> Self {
        Self {
            collection: None,
            tracks: Vec::new(),
            filter: String::new(),
            selected: 0,
            scroll_offset: 0,
            confirm_remove: false,
            rename_input: None,
        }
    }
}

impl CollectionDetailView {
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.tracks.len()).collect();
        }
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let artist = t.artist.as_deref().unwrap_or("");
                crate::tui::fuzzy::matches_any(&self.filter, &[&t.title, artist])
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected = 0;
        self.scroll_offset = 0;
    }
}

impl CollectionDetailView {
    pub fn load(&mut self, conn: &Connection, collection_id: i64) -> Result<()> {
        // Get collection info
        let collections = queries::list_collections(conn)?;
        self.collection = collections.into_iter().find(|c| c.id == collection_id);
        self.tracks = queries::get_collection_tracks(conn, collection_id, 0, 500)?;
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.confirm_remove = false;
        self.rename_input = None;
        Ok(())
    }

    pub fn has_popup(&self) -> bool {
        self.confirm_remove || self.rename_input.is_some()
    }

    /// Reload tracks, preserving cursor position (clamped to filtered list).
    fn reload_tracks(&mut self, conn: &Connection) {
        if let Some(coll) = &self.collection {
            if let Ok(tracks) = queries::get_collection_tracks(conn, coll.id, 0, 500) {
                self.tracks = tracks;
            }
            // Refresh collection metadata for track count/duration in header
            if let Ok(collections) = queries::list_collections(conn) {
                self.collection = collections.into_iter().find(|c| c.id == coll.id);
            }
        }
        let visible_len = self.filtered_indices().len();
        if visible_len == 0 {
            self.selected = 0;
        } else if self.selected >= visible_len {
            self.selected = visible_len - 1;
        }
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        conn: &Connection,
    ) -> CollectionDetailAction {
        // Rename mode captures input
        if let Some(input) = &mut self.rename_input {
            if keys::is_back(&key) {
                self.rename_input = None;
                return CollectionDetailAction::None;
            }
            if keys::is_confirm(&key) {
                let new_name = input.value.trim().to_string();
                if let Some(coll) = &self.collection {
                    if !new_name.is_empty() && new_name != coll.name {
                        let id = coll.id;
                        queries::rename_collection(conn, id, &new_name).ok();
                        // Reload collection info while preserving cursor
                        let prev = self.selected;
                        self.load(conn, id).ok();
                        if prev < self.tracks.len() {
                            self.selected = prev;
                        }
                    }
                }
                self.rename_input = None;
                return CollectionDetailAction::None;
            }
            input.handle_key(key);
            return CollectionDetailAction::None;
        }

        let visible = self.filtered_indices();

        // Confirmation popup captures input
        if self.confirm_remove {
            if key.code == KeyCode::Char('y') {
                let target = visible
                    .get(self.selected)
                    .and_then(|&i| self.tracks.get(i).map(|t| t.id));
                if let (Some(coll), Some(track_id)) = (&self.collection, target) {
                    let coll_id = coll.id;
                    queries::remove_track_from_collection(conn, coll_id, track_id).ok();
                    self.reload_tracks(conn);
                }
            }
            self.confirm_remove = false;
            return CollectionDetailAction::None;
        }

        let count = visible.len();

        if keys::is_up(&key) && self.selected > 0 {
            self.selected -= 1;
        }
        if keys::is_down(&key) && count > 0 && self.selected < count - 1 {
            self.selected += 1;
        }
        if keys::is_page_up(&key) {
            self.selected = self.selected.saturating_sub(20);
        }
        if keys::is_page_down(&key) && count > 0 {
            self.selected = (self.selected + 20).min(count - 1);
        }
        if keys::is_half_page_up(&key) {
            self.selected = self.selected.saturating_sub(10);
        }
        if keys::is_half_page_down(&key) && count > 0 {
            self.selected = (self.selected + 10).min(count - 1);
        }
        if key.code == KeyCode::Char('G') && count > 0 {
            self.selected = count - 1;
        }

        let current_track = visible
            .get(self.selected)
            .and_then(|&i| self.tracks.get(i));

        if key.code == KeyCode::Char('e') {
            if let Some(track) = current_track {
                return CollectionDetailAction::EditTrack(track.id);
            }
        }
        if key.code == KeyCode::Char('x') && current_track.is_some() {
            self.confirm_remove = true;
            return CollectionDetailAction::None;
        }
        if key.code == KeyCode::Char('R') {
            if let Some(coll) = &self.collection {
                let mut input =
                    TextInput::new("New collection name...").with_label(" Name: ");
                input.value = coll.name.clone();
                input.cursor = input.value.len();
                input.focused = true;
                self.rename_input = Some(input);
            }
            return CollectionDetailAction::None;
        }

        let _ = conn;
        CollectionDetailAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Min(5),   // tracks
            ])
            .split(area);

        // Header
        if let Some(coll) = &self.collection {
            let duration = format_duration_ms(coll.total_duration_ms);
            let header = Line::from(vec![
                Span::styled(
                    format!(" {} ", coll.name),
                    Style::default()
                        .fg(theme.accent_alt)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("· {} tracks · {}", coll.track_count, duration),
                    Style::default().fg(theme.fg_dim),
                ),
            ]);
            let p = Paragraph::new(header).block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.border)),
            );
            frame.render_widget(p, chunks[0]);
        }

        // Track table — iterate over filtered indices
        let visible = self.filtered_indices();
        let total = visible.len();
        let visible_height = chunks[1].height.saturating_sub(1) as usize;
        let scroll = if self.selected < self.scroll_offset {
            self.selected
        } else if self.selected + 1 >= self.scroll_offset + visible_height {
            (self.selected + 2).saturating_sub(visible_height)
        } else {
            self.scroll_offset
        };

        let mut rows = Vec::new();
        for pos in scroll..total.min(scroll + visible_height) {
            let i = visible[pos];
            let track = &self.tracks[i];
            let is_selected = pos == self.selected;

            let artist = track.artist.as_deref().unwrap_or("");
            let duration = track
                .duration_ms
                .map(|ms| {
                    let s = ms / 1000;
                    format!("{}:{:02}", s / 60, s % 60)
                })
                .unwrap_or_default();

            let row = Row::new(vec![
                Cell::from(track.title.clone()),
                Cell::from(artist.to_string()),
                Cell::from(duration),
                Cell::from(track.file_format.to_uppercase()),
            ]);

            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD)
            } else if pos % 2 == 0 {
                Style::default().bg(theme.bg).fg(theme.fg)
            } else {
                Style::default().bg(theme.bg_alt).fg(theme.fg)
            };

            rows.push(row.style(style));
        }

        let header = Row::new(vec![
            Cell::from(Span::styled("Title", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Artist", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Duration", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Fmt", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(35),
                Constraint::Percentage(30),
                Constraint::Length(8),
                Constraint::Length(6),
            ],
        )
        .header(header);

        frame.render_widget(table, chunks[1]);

        // Rename popup
        if let Some(input) = &self.rename_input {
            use crate::tui::widgets::popup;
            let content = vec![Line::from("")];
            let inner =
                popup::render_popup(frame, area, theme, "Rename Collection", &content, 50, 5);
            input.render(frame, inner, theme);
        }

        // Confirmation popup for removing a track
        if self.confirm_remove {
            use crate::tui::widgets::popup;
            let title = visible
                .get(self.selected)
                .and_then(|&i| self.tracks.get(i))
                .map(|t| t.title.clone())
                .unwrap_or_default();
            let content = vec![
                Line::from(format!("Remove '{}' from collection?", title)),
                Line::from(""),
                Line::from(Span::styled(
                    "y = yes, any other key = cancel",
                    Style::default().fg(theme.fg_muted),
                )),
            ];
            popup::render_popup(frame, area, theme, "Confirm Remove", &content, 60, 7);
        }
    }
}
