use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::config::Settings;
use crate::core::organizer::{self, DeleteCollectionPlan, remove_empty_parents};
use crate::core::pruner;
use crate::db::queries::{self, CollectionRow, TrackRow};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::selection::Selection;
use crate::tui::themes::Theme;
use crate::tui::views::library::format_duration_ms;
use crate::tui::widgets::confirm_delete::{ConfirmAction, ConfirmDelete};
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::track_table;

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
    /// User confirmed a batch track delete.
    Deleted,
}

enum InputMode {
    Normal,
    Creating(TextInput),
    Renaming {
        input: TextInput,
        id: i64,
    },
    ConfirmDelete {
        plan: DeleteCollectionPlan,
        widget: ConfirmDelete,
    },
    ConfirmBatchDelete {
        plans: Vec<DeleteCollectionPlan>,
        widget: ConfirmDelete,
    },
}

pub struct CollectionsView {
    pub collections: Vec<CollectionRow>,
    pub selected: usize,
    pub list_scroll_offset: usize,
    pub organize_plan: Option<crate::core::organizer::OrganizePlan>,
    pub organize_scroll: usize,
    pub organize_max_scroll: usize,
    pub organize_details: bool,
    pub selection: Selection,
    mode: InputMode,
}

impl Default for CollectionsView {
    fn default() -> Self {
        Self {
            collections: Vec::new(),
            selected: 0,
            list_scroll_offset: 0,
            organize_plan: None,
            organize_scroll: 0,
            organize_max_scroll: 0,
            organize_details: false,
            selection: Selection::default(),
            mode: InputMode::Normal,
        }
    }
}

impl CollectionsView {
    pub fn load(&mut self, conn: &Connection, search: Option<&str>) -> Result<()> {
        self.selection.clear();
        self.mode = InputMode::Normal;
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
        settings: &Settings,
    ) -> CollectionsAction {
        let file_delete_roots = organizer::file_delete_roots(settings);
        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                return CollectionsAction::OrganizeAll;
            }
            if keys::is_back(&key) {
                if self.organize_details {
                    self.organize_details = false;
                    self.organize_scroll = 0;
                } else {
                    self.organize_plan = None;
                    self.organize_scroll = 0;
                }
                return CollectionsAction::None;
            }
            if key.code == KeyCode::Char('d') {
                self.organize_details = !self.organize_details;
                self.organize_scroll = 0;
                return CollectionsAction::None;
            }
            let max = self.organize_max_scroll;
            if keys::is_down(&key) {
                self.organize_scroll = (self.organize_scroll + 1).min(max);
            }
            if keys::is_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(1);
            }
            if keys::is_page_down(&key) {
                self.organize_scroll = (self.organize_scroll + 20).min(max);
            }
            if keys::is_page_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(20);
            }
            if keys::is_half_page_down(&key) {
                self.organize_scroll = (self.organize_scroll + 10).min(max);
            }
            if keys::is_half_page_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(10);
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
                        organizer::apply_delete_collection_with_roots(
                            conn,
                            plan,
                            delete_files,
                            &file_delete_roots,
                        )
                        .ok();
                        self.mode = InputMode::Normal;
                        self.selection.clear();
                        return CollectionsAction::Refresh;
                    }
                }
            }
            InputMode::ConfirmBatchDelete { plans, widget } => {
                let action = widget.handle_key(key);
                match action {
                    ConfirmAction::None => return CollectionsAction::None,
                    ConfirmAction::Cancel => {
                        self.mode = InputMode::Normal;
                        return CollectionsAction::None;
                    }
                    ConfirmAction::Confirm { delete_files } => {
                        for plan in plans.iter() {
                            organizer::apply_delete_collection_with_roots(
                                conn,
                                plan,
                                delete_files,
                                &file_delete_roots,
                            )
                            .ok();
                        }
                        self.mode = InputMode::Normal;
                        self.selection.clear();
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
            && let Some(coll) = self.collections.get(self.selected)
        {
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
                let mut input = TextInput::new("New collection name...").with_label(" Name: ");
                input.value = coll.name.clone();
                input.cursor = input.value.len();
                input.focused = true;
                self.mode = InputMode::Renaming { input, id: coll.id };
            }
            return CollectionsAction::None;
        }
        if key.code == KeyCode::Char('O') {
            return CollectionsAction::OrganizeAll;
        }
        if keys::is_back(&key) && !self.selection.is_empty() {
            self.selection.clear();
            return CollectionsAction::None;
        }

        if keys::is_toggle_select(&key)
            && let Some(coll) = self.collections.get(self.selected)
        {
            self.selection.toggle(coll.id);
            if count > 0 && self.selected < count - 1 {
                self.selected += 1;
            }
            return CollectionsAction::None;
        }

        if keys::is_delete(&key) && !self.collections.is_empty() {
            let ids: Vec<i64> = if self.selection.is_empty() {
                self.collections
                    .get(self.selected)
                    .map(|c| vec![c.id])
                    .unwrap_or_default()
            } else {
                self.selection.ids()
            };
            if ids.is_empty() {
                return CollectionsAction::None;
            }

            if ids.len() == 1 {
                // Single-collection delete — preserve the existing rich popup.
                if let Ok(plan) =
                    organizer::plan_delete_collection_with_roots(conn, ids[0], &file_delete_roots)
                {
                    let mut widget = ConfirmDelete::new(
                        "Confirm Delete",
                        format!("Delete collection '{}'?", plan.collection_name),
                    );
                    let total_files = plan.files_to_delete.len();
                    if total_files > 0 {
                        widget = widget.with_summary(format!(
                            "{} file(s) inside the music directory",
                            total_files
                        ));
                    } else if !plan.files_outside_music_dir.is_empty() {
                        widget = widget.without_checkbox();
                        widget = widget.with_detail(format!(
                            "{} file(s) outside the music directory will be left on disk",
                            plan.files_outside_music_dir.len()
                        ));
                        widget = widget.with_detail(
                            "No files inside the managed directory — nothing to delete".to_string(),
                        );
                    } else {
                        widget = widget.without_checkbox();
                    }
                    if !plan.orphaned_track_ids.is_empty() {
                        let suffix = if total_files > 0 {
                            "; check the box to also delete files"
                        } else {
                            ""
                        };
                        widget = widget.with_warning(format!(
                            "{} track(s) only exist in this collection — they'll be removed from the library{}",
                            plan.orphaned_track_ids.len(), suffix
                        ));
                    }
                    if total_files > 0 && !plan.files_outside_music_dir.is_empty() {
                        widget = widget.with_detail(format!(
                            "{} file(s) outside the music directory will be left on disk",
                            plan.files_outside_music_dir.len()
                        ));
                    }
                    self.mode = InputMode::ConfirmDelete { plan, widget };
                }
            } else {
                // Batch — plan every collection and summarise.
                let mut plans = Vec::with_capacity(ids.len());
                for id in &ids {
                    if let Ok(plan) =
                        organizer::plan_delete_collection_with_roots(conn, *id, &file_delete_roots)
                    {
                        plans.push(plan);
                    }
                }
                if plans.is_empty() {
                    return CollectionsAction::None;
                }
                let total_files: usize = plans.iter().map(|p| p.files_to_delete.len()).sum();
                let total_orphans: usize = plans.iter().map(|p| p.orphaned_track_ids.len()).sum();
                let total_outside: usize =
                    plans.iter().map(|p| p.files_outside_music_dir.len()).sum();

                let mut widget = ConfirmDelete::new(
                    "Confirm Delete",
                    format!("Delete {} collections?", plans.len()),
                );
                if total_files > 0 {
                    widget = widget.with_summary(format!(
                        "{} file(s) inside the music directory",
                        total_files
                    ));
                } else if total_outside > 0 {
                    widget = widget.without_checkbox();
                    widget = widget.with_detail(format!(
                        "{} file(s) outside the music directory will be left on disk",
                        total_outside
                    ));
                    widget = widget.with_detail(
                        "No files inside the managed directory — nothing to delete".to_string(),
                    );
                } else {
                    widget = widget.without_checkbox();
                }
                const MAX_SHOWN: usize = 3;
                for plan in plans.iter().take(MAX_SHOWN) {
                    widget = widget.with_detail(format!(
                        "{} ({} file(s))",
                        plan.collection_name,
                        plan.files_to_delete.len()
                    ));
                }
                if plans.len() > MAX_SHOWN {
                    widget = widget.with_detail(format!(
                        "…and {} more collection(s)",
                        plans.len() - MAX_SHOWN
                    ));
                }
                if total_orphans > 0 {
                    let suffix = if total_files > 0 {
                        "; check the box to also delete files"
                    } else {
                        ""
                    };
                    widget = widget.with_warning(format!(
                        "{} track(s) only exist in these collections — they'll be removed from the library{}",
                        total_orphans, suffix
                    ));
                }
                if total_files > 0 && total_outside > 0 {
                    widget = widget.with_detail(format!(
                        "{} file(s) outside the music directory will be left on disk",
                        total_outside
                    ));
                }
                self.mode = InputMode::ConfirmBatchDelete { plans, widget };
            }
            return CollectionsAction::None;
        }

        CollectionsAction::None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if self.collections.is_empty() {
            let text = "No collections yet. Press 'n' to create one.";
            let line_w = text.chars().count() as u16;
            let y = area.y + area.height / 2;
            let x = area.x + area.width.saturating_sub(line_w) / 2;
            let w = line_w.min(area.width);
            let centered = Rect::new(x, y, w, 1);
            let msg = Paragraph::new(Span::styled(text, Style::default().fg(theme.fg_muted)))
                .alignment(Alignment::Center)
                .style(Style::default().bg(theme.bg));
            frame.render_widget(msg, centered);
            return;
        }

        // Inner area below the top border holds the header + rows.
        // visible_height = inner.height - 1 (header).
        let inner_height = area.height.saturating_sub(1) as usize; // -1 for top border
        let visible_height = inner_height.saturating_sub(1);
        self.list_scroll_offset = crate::tui::views::library::compute_scroll_offset(
            self.selected,
            self.list_scroll_offset,
            visible_height,
        );
        let scroll = self.list_scroll_offset;

        let mut rows = Vec::new();
        for i in scroll..self.collections.len().min(scroll + visible_height) {
            let coll = &self.collections[i];
            let is_selected = i == self.selected;
            let duration = format_duration_ms(coll.total_duration_ms);

            let gutter_span = if self.selection.contains(coll.id) {
                Span::styled("▎", Style::default().fg(theme.accent))
            } else {
                Span::raw(" ")
            };
            let row = Row::new(vec![
                Cell::from(gutter_span),
                Cell::from(coll.name.clone()),
                track_table::count_cell(coll.track_count),
                track_table::duration_cell(duration),
            ]);

            rows.push(row.style(track_table::row_style(theme, i, is_selected)));
        }

        let header = Row::new(vec![
            Cell::from(" "),
            track_table::header_cell("Collection", theme),
            track_table::count_header_cell("Tracks", theme),
            track_table::duration_header_cell("Duration", theme),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(rows, track_table::collection_list_widths())
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
        if let InputMode::ConfirmBatchDelete { widget, .. } = &self.mode {
            widget.render(frame, area, theme);
        }

        // Organize popup
        if let Some(plan) = &self.organize_plan {
            use crate::tui::widgets::organize_popup::{self, OrganizeView};
            let nothing = plan.moves.is_empty() && plan.copies.is_empty();
            let (view, title, hint, width) = if self.organize_details {
                (
                    OrganizeView::Details,
                    "Organize Library — Details",
                    "Enter = apply · d = summary · j/k = scroll · Esc = back",
                    90,
                )
            } else if nothing {
                (OrganizeView::Summary, "Organize Library", "Esc = close", 85)
            } else {
                (
                    OrganizeView::Summary,
                    "Organize Library",
                    "Enter = apply · d = details · j/k = scroll · Esc = cancel",
                    85,
                )
            };
            self.organize_max_scroll = organize_popup::render(
                frame,
                area,
                theme,
                plan,
                &mut self.organize_scroll,
                view,
                title,
                hint,
                width,
            );
        }
    }

    pub fn render_detail_bar(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if let Some(coll) = self.collections.get(self.selected) {
            let duration = format_duration_ms(coll.total_duration_ms);
            let detail = format!(
                " {} · {} tracks · {}",
                coll.name, coll.track_count, duration
            );
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
    pub organize_scroll: usize,
    pub organize_max_scroll: usize,
    pub organize_details: bool,
    pub notice: Option<String>,
    pub filter: String,
    pub selected: usize,
    pub scroll_offset: usize,
    confirm_remove: Option<RemoveTrackConfirm>,
    rename_input: Option<TextInput>,
    pub selection: Selection,
    pending_delete: Option<(pruner::DeletePlan, ConfirmDelete)>,
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
            organize_scroll: 0,
            organize_max_scroll: 0,
            organize_details: false,
            notice: None,
            filter: String::new(),
            selected: 0,
            scroll_offset: 0,
            confirm_remove: None,
            rename_input: None,
            selection: Selection::default(),
            pending_delete: None,
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
    pub fn load(&mut self, conn: &Connection, collection_id: i64, music_dir: &Path) -> Result<()> {
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
        self.selection.clear();
        self.pending_delete = None;
        Ok(())
    }

    pub fn has_popup(&self) -> bool {
        self.confirm_remove.is_some()
            || self.rename_input.is_some()
            || self.organize_plan.is_some()
            || self.pending_delete.is_some()
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
        settings: &Settings,
    ) -> CollectionDetailAction {
        let file_delete_roots = organizer::file_delete_roots(settings);
        // Delete-confirm popup captures input (batch track delete).
        if let Some((plan, popup)) = &mut self.pending_delete {
            match popup.handle_key(key) {
                ConfirmAction::None => return CollectionDetailAction::None,
                ConfirmAction::Cancel => {
                    self.pending_delete = None;
                    return CollectionDetailAction::None;
                }
                ConfirmAction::Confirm { delete_files } => {
                    let file_delete_roots = organizer::file_delete_roots(settings);
                    let plan = plan.clone();
                    self.pending_delete = None;
                    let _ =
                        pruner::apply_delete_plan(conn, &plan, delete_files, &file_delete_roots);
                    self.selection.clear();
                    self.reload_tracks(conn);
                    return CollectionDetailAction::Deleted;
                }
            }
        }

        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                return CollectionDetailAction::Organize;
            }
            if keys::is_back(&key) {
                if self.organize_details {
                    self.organize_details = false;
                    self.organize_scroll = 0;
                } else {
                    self.organize_plan = None;
                    self.organize_scroll = 0;
                }
                return CollectionDetailAction::None;
            }
            if key.code == KeyCode::Char('d') {
                self.organize_details = !self.organize_details;
                self.organize_scroll = 0;
                return CollectionDetailAction::None;
            }
            let max = self.organize_max_scroll;
            if keys::is_down(&key) {
                self.organize_scroll = (self.organize_scroll + 1).min(max);
            }
            if keys::is_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(1);
            }
            if keys::is_page_down(&key) {
                self.organize_scroll = (self.organize_scroll + 20).min(max);
            }
            if keys::is_page_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(20);
            }
            if keys::is_half_page_down(&key) {
                self.organize_scroll = (self.organize_scroll + 10).min(max);
            }
            if keys::is_half_page_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(10);
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
                    && !new_name.is_empty()
                    && new_name != coll.name
                {
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

                        if delete_files
                            && let Some(p) = &file_path
                            && p.exists()
                            && path_is_in_roots(p, &file_delete_roots)
                        {
                            let _ = std::fs::remove_file(p);
                            if let Some(parent) = p.parent() {
                                let _ = remove_empty_parents(parent, &file_delete_roots);
                            }
                        }

                        // If removing this collection membership would leave
                        // the track homeless, remove it from the library by
                        // default instead of creating an accidental loose row.
                        if will_orphan {
                            queries::delete_track(conn, track_id).ok();
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

        let current_track = visible.get(self.selected).and_then(|&i| self.tracks.get(i));

        if keys::is_back(&key) && !self.selection.is_empty() {
            self.selection.clear();
            return CollectionDetailAction::None;
        }

        if keys::is_toggle_select(&key)
            && let Some(track) = current_track
        {
            self.selection.toggle(track.id);
            if count > 0 && self.selected < count - 1 {
                self.selected += 1;
            }
            return CollectionDetailAction::None;
        }

        if keys::is_delete(&key) {
            let ids: Vec<i64> = if self.selection.is_empty() {
                current_track.map(|t| vec![t.id]).unwrap_or_default()
            } else {
                self.selection.ids()
            };
            if ids.is_empty() {
                return CollectionDetailAction::None;
            }
            let file_delete_roots = organizer::file_delete_roots(settings);
            match pruner::plan_delete_tracks(conn, &ids, &file_delete_roots) {
                Ok(plan) => {
                    if plan.is_empty() {
                        return CollectionDetailAction::None;
                    }
                    let popup = build_collection_track_confirm(&plan);
                    self.pending_delete = Some((plan, popup));
                }
                Err(e) => {
                    self.notice = Some(format!("Delete plan failed: {}", e));
                }
            }
            return CollectionDetailAction::None;
        }

        if key.code == KeyCode::Char('e')
            && let Some(track) = current_track
        {
            return CollectionDetailAction::EditTrack(track.id);
        }
        if key.code == KeyCode::Char('x') {
            if let Some(track) = current_track
                && let Some(coll) = &self.collection
            {
                // Look up the collection_file_path and other-homes info
                let homes = queries::get_collection_tracks_with_other_homes(conn, coll.id)
                    .unwrap_or_default();
                let info = homes.iter().find(|h| h.track_id == track.id);
                let will_orphan = info
                    .map(|i| !i.has_album && i.other_collection_count == 0)
                    .unwrap_or(false);
                let file_path = info
                    .and_then(|i| {
                        i.collection_file_path
                            .clone()
                            .or_else(|| will_orphan.then(|| i.track_file_path.clone()))
                    })
                    .map(PathBuf::from);
                let can_delete_file = file_path
                    .as_ref()
                    .is_some_and(|p| path_is_in_roots(p, &file_delete_roots));

                let mut widget = ConfirmDelete::new(
                    "Confirm Remove",
                    format!("Remove '{}' from collection '{}'?", track.title, coll.name),
                );
                if let Some(p) = &file_path {
                    if can_delete_file {
                        widget = widget.with_summary(format!("File: {}", p.display()));
                    } else {
                        widget = widget.without_checkbox();
                        widget = widget.with_detail(format!(
                            "File is outside the managed directory — kyoku won't delete it",
                        ));
                    }
                } else {
                    widget = widget.without_checkbox();
                }
                if will_orphan {
                    let suffix = if can_delete_file {
                        "; check the box to also delete its file"
                    } else {
                        "; no file is eligible for deletion"
                    };
                    widget = widget.with_warning(format!(
                        "This is the only place this track lives — it will be removed from the library{}",
                        suffix
                    ));
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
                let mut input = TextInput::new("New collection name...").with_label(" Name: ");
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

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Min(5),    // tracks
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
        self.scroll_offset = crate::tui::views::library::compute_scroll_offset(
            self.selected,
            self.scroll_offset,
            visible_height,
        );
        let scroll = self.scroll_offset;

        let mut rows = Vec::new();
        for (pos, &i) in visible
            .iter()
            .enumerate()
            .take(total.min(scroll + visible_height))
            .skip(scroll)
        {
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

            let gutter_span = if self.selection.contains(track.id) {
                Span::styled("▎", Style::default().fg(theme.accent))
            } else {
                Span::raw(" ")
            };
            let row = Row::new(vec![
                Cell::from(gutter_span),
                track_table::numeric_cell(i + 1),
                Cell::from(track.title.clone()),
                Cell::from(artist.to_string()),
                Cell::from(duration),
                Cell::from(track.file_format.to_uppercase()),
            ]);

            rows.push(row.style(track_table::row_style(theme, pos, is_selected)));
        }

        let header = Row::new(vec![
            Cell::from(" "),
            track_table::numeric_header_cell("#", theme),
            track_table::header_cell("Title", theme),
            track_table::header_cell("Artist", theme),
            track_table::header_cell("Duration", theme),
            track_table::header_cell("Fmt", theme),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(rows, track_table::collection_detail_widths()).header(header);

        frame.render_widget(table, chunks[1]);

        // Footer: selected track's path and its location status
        let footer_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[2]);

        if let Some(selected_track) = visible.get(self.selected).and_then(|&i| self.tracks.get(i)) {
            // Line 1: notice (if set) or status badge
            if let Some(notice) = &self.notice {
                let p = Paragraph::new(Span::styled(
                    format!(" {} ", notice),
                    Style::default().fg(theme.yellow),
                ))
                .style(Style::default().bg(theme.bg_alt));
                frame.render_widget(p, footer_chunks[0]);
            } else {
                let has_collection_copy = self.collection_paths.contains_key(&selected_track.id);
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

        // Delete-confirm popup (batch/track delete)
        if let Some((_, popup)) = &self.pending_delete {
            popup.render(frame, area, theme);
        }

        // Organize preview popup
        if let Some(plan) = &self.organize_plan {
            use crate::tui::widgets::organize_popup::{self, OrganizeView};
            let coll_name = self
                .collection
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("?");
            let nothing = plan.moves.is_empty() && plan.copies.is_empty();
            let (view, title, hint, width) = if self.organize_details {
                (
                    OrganizeView::Details,
                    format!("Organize — {} — Details", coll_name),
                    "Enter = apply · d = summary · j/k = scroll · Esc = back",
                    90,
                )
            } else if nothing {
                (
                    OrganizeView::Summary,
                    format!("Organize — {}", coll_name),
                    "Esc = close",
                    85,
                )
            } else {
                (
                    OrganizeView::Summary,
                    format!("Organize — {}", coll_name),
                    "Enter = apply · d = details · j/k = scroll · Esc = cancel",
                    85,
                )
            };
            self.organize_max_scroll = organize_popup::render(
                frame,
                area,
                theme,
                plan,
                &mut self.organize_scroll,
                view,
                &title,
                hint,
                width,
            );
        }
    }
}

fn path_is_in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path.starts_with(root) && path != root.as_path())
}

fn build_collection_track_confirm(plan: &pruner::DeletePlan) -> ConfirmDelete {
    let n = plan.track_ids.len();
    let primary = if n == 1 {
        "Delete 1 track from library?".to_string()
    } else {
        format!("Delete {} tracks from library?", n)
    };
    let summary = format!("{} file(s) on disk", plan.deletable_file_count());
    let mut popup = ConfirmDelete::new("Confirm delete", primary).with_summary(summary);
    for line in &plan.album_summary_lines {
        popup = popup.with_detail(line.clone());
    }
    if plan.additional_albums > 0 {
        popup = popup.with_detail(format!("…and {} more album(s)", plan.additional_albums));
    }
    if !plan.collection_copies_to_delete.is_empty() {
        popup = popup.with_warning(format!(
            "{} collection copy file(s) will also be removed",
            plan.collection_copies_to_delete.len()
        ));
    }
    if plan.deletable_file_count() == 0 && plan.files_outside_managed.is_empty() {
        popup = popup.without_checkbox();
    } else if plan.deletable_file_count() == 0 {
        popup = popup.without_checkbox();
        popup = popup.with_detail(format!(
            "{} file(s) outside the music directory will be left on disk",
            plan.files_outside_managed.len()
        ));
        popup = popup.with_detail(
            "No files inside the managed directory — nothing to delete".to_string(),
        );
    } else {
        if !plan.files_outside_managed.is_empty() {
            popup = popup.with_detail(format!(
                "{} file(s) outside the music directory will be left on disk",
                plan.files_outside_managed.len()
            ));
        }
        popup = popup.with_checkbox_label(format!(
            "Also delete {} file(s) from disk",
            plan.deletable_file_count()
        ));
    }
    popup
}
