use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::core::organizer::{self, remove_empty_parents, DeleteCollectionPlan};
use crate::db::queries::{self, CollectionRow, TrackRow};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::views::library::format_duration_ms;
use crate::tui::widgets::confirm_delete::{ConfirmAction, ConfirmDelete};
use crate::tui::widgets::input::TextInput;

pub enum CollectionsAction {
    None,
    OpenCollection(i64),
    Refresh,
    OrganizeAll,
}

pub enum CollectionDetailAction {
    None,
    EditTrack(i64),
    Organize,
    OpenDir,
}

enum InputMode {
    Normal,
    Creating(TextInput),
    Renaming { input: TextInput, id: i64 },
    ConfirmDelete {
        plan: DeleteCollectionPlan,
        widget: ConfirmDelete,
    },
}

pub struct CollectionsView {
    pub collections: Vec<CollectionRow>,
    pub selected: usize,
    pub organize_plan: Option<crate::core::organizer::OrganizePlan>,
    mode: InputMode,
}

impl Default for CollectionsView {
    fn default() -> Self {
        Self {
            collections: Vec::new(),
            selected: 0,
            organize_plan: None,
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

    pub fn has_popup(&self) -> bool {
        !matches!(self.mode, InputMode::Normal) || self.organize_plan.is_some()
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        conn: &Connection,
        music_dir: &Path,
    ) -> CollectionsAction {
        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                return CollectionsAction::OrganizeAll;
            }
            if keys::is_back(&key) {
                self.organize_plan = None;
            }
            return CollectionsAction::None;
        }

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
            InputMode::ConfirmDelete { plan, widget } => {
                let action = widget.handle_key(key);
                match action {
                    ConfirmAction::None => return CollectionsAction::None,
                    ConfirmAction::Cancel => {
                        self.mode = InputMode::Normal;
                        return CollectionsAction::None;
                    }
                    ConfirmAction::Confirm { delete_files } => {
                        organizer::apply_delete_collection(conn, plan, delete_files, music_dir)
                            .ok();
                        self.mode = InputMode::Normal;
                        return CollectionsAction::Refresh;
                    }
                }
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
        if keys::is_confirm(&key)
            && let Some(coll) = self.collections.get(self.selected) {
                return CollectionsAction::OpenCollection(coll.id);
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
        if key.code == KeyCode::Char('O') {
            return CollectionsAction::OrganizeAll;
        }
        if key.code == KeyCode::Char('d') && !self.collections.is_empty() {
            if let Some(coll) = self.collections.get(self.selected)
                && let Ok(plan) = organizer::plan_delete_collection(conn, coll.id, music_dir) {
                    let mut widget = ConfirmDelete::new(
                        "Confirm Delete",
                        format!("Delete collection '{}'?", plan.collection_name),
                    );

                    let total_files = plan.files_to_delete.len();
                    if total_files > 0 {
                        widget = widget.with_summary(format!(
                            "{} file(s) under {}",
                            total_files,
                            music_dir.display()
                        ));
                    } else {
                        widget = widget.without_checkbox();
                    }

                    if !plan.orphaned_track_ids.is_empty() {
                        widget = widget.with_warning(format!(
                            "{} track(s) only exist in this collection — they'll be \
                             removed entirely if you delete files",
                            plan.orphaned_track_ids.len()
                        ));
                    }

                    if !plan.files_outside_music_dir.is_empty() {
                        widget = widget.with_warning(format!(
                            "{} file(s) outside music_dir will NOT be touched",
                            plan.files_outside_music_dir.len()
                        ));
                    }

                    self.mode = InputMode::ConfirmDelete { plan, widget };
                }
            return CollectionsAction::None;
        }

        let _ = music_dir;
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

        // Render delete confirmation widget
        if let InputMode::ConfirmDelete { widget, .. } = &self.mode {
            widget.render(frame, area, theme);
        }

        // Organize popup
        if let Some(plan) = &self.organize_plan {
            use crate::tui::widgets::popup;

            let mut lines = vec![Line::from("")];
            if plan.moves.is_empty() && plan.copies.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "Nothing to organize — {} file(s) already in place.",
                        plan.skipped
                    ),
                    Style::default().fg(theme.fg_muted),
                )));
            } else {
                if !plan.moves.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!(" {} file(s) to move", plan.moves.len()),
                        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                    )));
                }
                if !plan.copies.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!(" {} collection copy/copies", plan.copies.len()),
                        Style::default().fg(theme.fg),
                    )));
                }
            }
            if plan.skipped > 0 && !(plan.moves.is_empty() && plan.copies.is_empty()) {
                lines.push(Line::from(Span::styled(
                    format!(" {} already in place", plan.skipped),
                    Style::default().fg(theme.fg_muted),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                if plan.moves.is_empty() && plan.copies.is_empty() {
                    "Esc = close"
                } else {
                    "Enter = apply · Esc = cancel"
                },
                Style::default().fg(theme.fg_muted),
            )));

            let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
            popup::render_popup(frame, area, theme, "Organize Library", &lines, 70, height);
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
    /// Maps track_id → collection_file_path (if the track has an organized
    /// copy inside the collection folder). Populated on load.
    pub collection_paths: std::collections::HashMap<i64, String>,
    /// Cached music_dir for detecting whether a track's file_path is
    /// inside the organized library or still in the inbox.
    pub music_dir: PathBuf,
    pub organize_plan: Option<crate::core::organizer::OrganizePlan>,
    pub notice: Option<String>,
    pub filter: String,
    pub selected: usize,
    pub scroll_offset: usize,
    confirm_remove: Option<RemoveTrackConfirm>,
    rename_input: Option<TextInput>,
}

struct RemoveTrackConfirm {
    track_id: i64,
    file_path: Option<PathBuf>,
    will_orphan: bool,
    widget: ConfirmDelete,
}

impl Default for CollectionDetailView {
    fn default() -> Self {
        Self {
            collection: None,
            tracks: Vec::new(),
            collection_paths: std::collections::HashMap::new(),
            music_dir: PathBuf::new(),
            organize_plan: None,
            notice: None,
            filter: String::new(),
            selected: 0,
            scroll_offset: 0,
            confirm_remove: None,
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
    pub fn load(
        &mut self,
        conn: &Connection,
        collection_id: i64,
        music_dir: &Path,
    ) -> Result<()> {
        // Get collection info
        let collections = queries::list_collections(conn)?;
        self.collection = collections.into_iter().find(|c| c.id == collection_id);
        self.tracks = queries::get_collection_tracks(conn, collection_id, 0, 500)?;
        self.collection_paths =
            queries::get_collection_file_paths(conn, collection_id).unwrap_or_default();
        self.music_dir = music_dir.to_path_buf();
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.confirm_remove = None;
        self.rename_input = None;
        Ok(())
    }

    pub fn has_popup(&self) -> bool {
        self.confirm_remove.is_some()
            || self.rename_input.is_some()
            || self.organize_plan.is_some()
    }

    /// Reload tracks, preserving cursor position (clamped to filtered list).
    fn reload_tracks(&mut self, conn: &Connection) {
        if let Some(coll) = &self.collection {
            let coll_id = coll.id;
            if let Ok(tracks) = queries::get_collection_tracks(conn, coll_id, 0, 500) {
                self.tracks = tracks;
            }
            self.collection_paths =
                queries::get_collection_file_paths(conn, coll_id).unwrap_or_default();
            // Refresh collection metadata for track count/duration in header
            if let Ok(collections) = queries::list_collections(conn) {
                self.collection = collections.into_iter().find(|c| c.id == coll_id);
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
        music_dir: &Path,
    ) -> CollectionDetailAction {
        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                return CollectionDetailAction::Organize;
            }
            if keys::is_back(&key) {
                self.organize_plan = None;
            }
            return CollectionDetailAction::None;
        }

        // Rename mode captures input
        if let Some(input) = &mut self.rename_input {
            if keys::is_back(&key) {
                self.rename_input = None;
                return CollectionDetailAction::None;
            }
            if keys::is_confirm(&key) {
                let new_name = input.value.trim().to_string();
                if let Some(coll) = &self.collection
                    && !new_name.is_empty() && new_name != coll.name {
                        let id = coll.id;
                        queries::rename_collection(conn, id, &new_name).ok();
                        // Reload collection info while preserving cursor
                        let prev = self.selected;
                        let cached_music_dir = self.music_dir.clone();
                        self.load(conn, id, &cached_music_dir).ok();
                        if prev < self.tracks.len() {
                            self.selected = prev;
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
        if let Some(state) = &mut self.confirm_remove {
            let action = state.widget.handle_key(key);
            match action {
                ConfirmAction::None => return CollectionDetailAction::None,
                ConfirmAction::Cancel => {
                    self.confirm_remove = None;
                    return CollectionDetailAction::None;
                }
                ConfirmAction::Confirm { delete_files } => {
                    let track_id = state.track_id;
                    let file_path = state.file_path.clone();
                    let will_orphan = state.will_orphan;
                    self.confirm_remove = None;

                    if let Some(coll) = &self.collection {
                        let coll_id = coll.id;
                        queries::remove_track_from_collection(conn, coll_id, track_id).ok();

                        if delete_files {
                            if let Some(p) = &file_path
                                && p.exists() && p.starts_with(music_dir) {
                                    let _ = std::fs::remove_file(p);
                                    if let Some(parent) = p.parent() {
                                        let roots = [music_dir.to_path_buf()];
                                        let _ = remove_empty_parents(parent, &roots);
                                    }
                                }
                            // If removing this would leave the track with no
                            // file home, delete the track entirely.
                            if will_orphan {
                                queries::delete_track(conn, track_id).ok();
                            }
                        }

                        self.reload_tracks(conn);
                    }
                    return CollectionDetailAction::None;
                }
            }
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

        if key.code == KeyCode::Char('e')
            && let Some(track) = current_track {
                return CollectionDetailAction::EditTrack(track.id);
            }
        if key.code == KeyCode::Char('x') {
            if let Some(track) = current_track
                && let Some(coll) = &self.collection {
                    // Look up the collection_file_path and other-homes info
                    let homes = queries::get_collection_tracks_with_other_homes(conn, coll.id)
                        .unwrap_or_default();
                    let info = homes.iter().find(|h| h.track_id == track.id);
                    let file_path = info.and_then(|i| i.collection_file_path.clone()).map(PathBuf::from);
                    let will_orphan = info
                        .map(|i| !i.has_album && i.other_collection_count == 0)
                        .unwrap_or(false);

                    let mut widget = ConfirmDelete::new(
                        "Confirm Remove",
                        format!("Remove '{}' from collection '{}'?", track.title, coll.name),
                    );
                    if let Some(p) = &file_path {
                        if p.starts_with(music_dir) {
                            widget = widget.with_summary(format!("File: {}", p.display()));
                        } else {
                            widget = widget.without_checkbox();
                        }
                    } else {
                        widget = widget.without_checkbox();
                    }
                    if will_orphan {
                        widget = widget.with_warning(
                            "This is the only place this track lives — \
                             checking the box will remove it from the library entirely",
                        );
                    }

                    self.confirm_remove = Some(RemoveTrackConfirm {
                        track_id: track.id,
                        file_path,
                        will_orphan,
                        widget,
                    });
                }
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
        if key.code == KeyCode::Char('O') {
            return CollectionDetailAction::Organize;
        }
        #[cfg(not(target_os = "windows"))]
        if key.code == KeyCode::Char('o') {
            return CollectionDetailAction::OpenDir;
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
                Constraint::Length(2), // footer (status + path)
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

        // Footer: selected track's path and its location status
        let footer_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[2]);

        if let Some(selected_track) = visible
            .get(self.selected)
            .and_then(|&i| self.tracks.get(i))
        {
            // Line 1: notice (if set) or status badge
            if let Some(notice) = &self.notice {
                let p = Paragraph::new(Span::styled(
                    format!(" {} ", notice),
                    Style::default().fg(theme.yellow),
                ))
                .style(Style::default().bg(theme.bg_alt));
                frame.render_widget(p, footer_chunks[0]);
            } else {
                let has_collection_copy =
                    self.collection_paths.contains_key(&selected_track.id);
                let track_path = Path::new(&selected_track.file_path);
                let track_in_library = !self.music_dir.as_os_str().is_empty()
                    && track_path.starts_with(&self.music_dir);
                let (badge, badge_color, desc) = if has_collection_copy {
                    (
                        "[copy]",
                        theme.green,
                        "organized collection copy exists on disk",
                    )
                } else if track_in_library {
                    (
                        "[linked]",
                        theme.cyan,
                        "linked to album/loose file — collection copy will be created on organize",
                    )
                } else {
                    (
                        "[inbox]",
                        theme.yellow,
                        "still in inbox — run `kyoku organize` to move it",
                    )
                };
                let status = Line::from(vec![
                    Span::styled(
                        format!(" {} ", badge),
                        Style::default()
                            .fg(badge_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc, Style::default().fg(theme.fg_dim)),
                ]);
                let p = Paragraph::new(status).style(Style::default().bg(theme.bg_alt));
                frame.render_widget(p, footer_chunks[0]);
            }

            // Line 2: the actual path
            let path_display = self
                .collection_paths
                .get(&selected_track.id)
                .cloned()
                .unwrap_or_else(|| selected_track.file_path.clone());
            let p = Paragraph::new(Span::styled(
                format!(" {}", path_display),
                Style::default().fg(theme.fg_muted),
            ))
            .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, footer_chunks[1]);
        }

        // Rename popup
        if let Some(input) = &self.rename_input {
            use crate::tui::widgets::popup;
            let content = vec![Line::from("")];
            let inner =
                popup::render_popup(frame, area, theme, "Rename Collection", &content, 50, 5);
            input.render(frame, inner, theme);
        }

        // Confirmation popup for removing a track
        if let Some(state) = &self.confirm_remove {
            state.widget.render(frame, area, theme);
        }

        // Organize preview popup
        if let Some(plan) = &self.organize_plan {
            use crate::tui::widgets::popup;

            let coll_name = self
                .collection
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("?");

            let mut lines = vec![Line::from("")];

            if plan.moves.is_empty() && plan.copies.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "Nothing to organize — {} file(s) already in place.",
                        plan.skipped
                    ),
                    Style::default().fg(theme.fg_muted),
                )));
            } else {
                if !plan.moves.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!(" {} file(s) to move:", plan.moves.len()),
                        Style::default()
                            .fg(theme.fg)
                            .add_modifier(Modifier::BOLD),
                    )));
                    for m in plan.moves.iter().take(8) {
                        let name =
                            m.to.file_name().and_then(|f| f.to_str()).unwrap_or("?");
                        lines.push(Line::from(Span::styled(
                            format!("   {}", name),
                            Style::default().fg(theme.fg_dim),
                        )));
                    }
                    if plan.moves.len() > 8 {
                        lines.push(Line::from(Span::styled(
                            format!("   … and {} more", plan.moves.len() - 8),
                            Style::default().fg(theme.fg_muted),
                        )));
                    }
                }
                if !plan.copies.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!(" {} collection copy/copies", plan.copies.len()),
                        Style::default().fg(theme.fg),
                    )));
                }
            }

            if plan.skipped > 0 {
                lines.push(Line::from(Span::styled(
                    format!(" {} already in place", plan.skipped),
                    Style::default().fg(theme.fg_muted),
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Enter = apply · Esc = cancel",
                Style::default().fg(theme.fg_muted),
            )));

            let title = format!("Organize — {}", coll_name);
            let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
            popup::render_popup(frame, area, theme, &title, &lines, 80, height);
        }
    }
}
