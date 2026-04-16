use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rusqlite::Connection;

use crate::config::Settings;
use crate::db::queries;

use super::keybindings as keys;
use super::themes::Theme;
use super::views::collections::{CollectionDetailView, CollectionsView};
use super::views::detail::AlbumDetailView;
use super::views::edit::EditorView;
use super::views::global_search::{GlobalSearchAction, GlobalSearchView};
use super::views::help::HelpOverlay;
use super::views::import::ImportView;
use super::views::library::LibraryView;
use super::widgets::input::TextInput;

#[derive(Debug, Clone, PartialEq)]
pub enum AppView {
    Library,
    Collections,
    AlbumDetail { album_id: i64 },
    LooseTracks,
    CollectionDetail { collection_id: i64 },
    Import,
    Editor { track_id: i64 },
    Help,
}

pub enum AppAction {
    None,
    ChangeView(AppView),
    Quit,
}

pub struct App {
    pub view: AppView,
    pub theme: &'static Theme,
    pub conn: Connection,
    pub settings: Settings,
    pub inbox_count: usize,
    pub track_count: i64,
    pub should_quit: bool,

    // View states
    pub library: LibraryView,
    pub collections: CollectionsView,
    pub album_detail: AlbumDetailView,
    pub collection_detail: CollectionDetailView,
    pub import: ImportView,
    pub editor: EditorView,
    pub help: HelpOverlay,
    pub global_search: GlobalSearchView,
    pub global_search_open: bool,

    // Search
    pub search: TextInput,
    pub search_debounce: Option<Instant>,

    // View to return to when leaving the editor
    editor_return_to: Option<AppView>,
}

impl App {
    pub fn new(conn: Connection, settings: Settings, theme: &'static Theme) -> Self {
        let track_count = queries::count_tracks(&conn).unwrap_or(0);

        let mut app = Self {
            view: AppView::Library,
            theme,
            conn,
            settings,
            inbox_count: 0,
            track_count,
            should_quit: false,

            library: LibraryView::default(),
            collections: CollectionsView::default(),
            album_detail: AlbumDetailView::default(),
            collection_detail: CollectionDetailView::default(),
            import: ImportView::default(),
            editor: EditorView::default(),
            help: HelpOverlay::default(),
            global_search: GlobalSearchView::default(),
            global_search_open: false,

            search: TextInput::new("Search..."),
            search_debounce: None,

            editor_return_to: None,
        };

        // Initial data load
        app.library.load(&app.conn, None).ok();
        app
    }

    pub fn handle_event(&mut self, event: Event) -> AppAction {
        let Event::Key(key) = event else {
            return AppAction::None;
        };

        // Skip key release events
        if key.kind != event::KeyEventKind::Press {
            return AppAction::None;
        }

        // Help overlay captures all input when visible
        if self.help.visible {
            return self.help.handle_key(key);
        }

        // Global search captures all input when open
        if self.global_search_open {
            let action = self.global_search.handle_key(key, &self.conn);
            return self.handle_global_search_action(action);
        }

        // Global keys (suppressed when a view popup is accepting text input)
        let view_captures_input = self.current_view_has_popup();
        if !self.search.focused && !view_captures_input {
            if keys::is_quit(&key) {
                return AppAction::Quit;
            }
            if keys::is_help(&key) {
                self.help.visible = true;
                return AppAction::None;
            }
            if keys::is_refresh(&key) {
                self.refresh();
                return AppAction::None;
            }
        }

        // Search bar handling
        if self.search.focused {
            if keys::is_back(&key) {
                self.search.focused = false;
                self.search.clear();
                self.on_search_changed();
                return AppAction::None;
            }
            if keys::is_confirm(&key) {
                self.search.focused = false;
                return AppAction::None;
            }
            if self.search.handle_key(key) {
                self.search_debounce = Some(Instant::now() + Duration::from_millis(150));
            }
            return AppAction::None;
        }

        if !view_captures_input && keys::is_search_focus(&key) {
            self.search.focused = true;
            return AppAction::None;
        }

        // Global search: g opens it from anywhere
        if !view_captures_input && key.code == KeyCode::Char('g') {
            self.global_search.open();
            self.global_search_open = true;
            return AppAction::None;
        }

        // Delegate to current view
        match &self.view {
            AppView::Library => self.handle_library_key(key),
            AppView::Collections => self.handle_collections_key(key),
            AppView::AlbumDetail { .. } | AppView::LooseTracks => self.handle_detail_key(key),
            AppView::CollectionDetail { .. } => self.handle_collection_detail_key(key),
            AppView::Import => self.handle_import_key(key),
            AppView::Editor { .. } => self.handle_editor_key(key),
            AppView::Help => AppAction::None,
        }
    }

    /// Returns true if the current view has a modal popup that should capture text input.
    fn current_view_has_popup(&self) -> bool {
        match self.view {
            AppView::Library => self.library.has_popup(),
            AppView::AlbumDetail { .. } | AppView::LooseTracks => {
                self.album_detail.has_popup()
            }
            AppView::Collections => self.collections.has_popup(),
            AppView::CollectionDetail { .. } => self.collection_detail.has_popup(),
            AppView::Editor { .. } => self.editor.is_editing(),
            AppView::Import => self.import.is_capturing_input(),
            _ => false,
        }
    }

    pub fn tick(&mut self) {
        // Check debounced search
        if let Some(deadline) = self.search_debounce {
            if Instant::now() >= deadline {
                self.on_search_changed();
                self.search_debounce = None;
            }
        }

        // Tick import view for background operations
        if self.view == AppView::Import {
            self.import.tick(&self.conn);
        }
    }

    fn on_search_changed(&mut self) {
        let query = if self.search.value.is_empty() {
            None
        } else {
            Some(self.search.value.as_str())
        };
        match self.view {
            AppView::Library => {
                self.library.load(&self.conn, query).ok();
            }
            AppView::Collections => {
                self.collections.load(&self.conn, query).ok();
            }
            AppView::AlbumDetail { .. } | AppView::LooseTracks => {
                self.album_detail
                    .set_filter(self.search.value.clone());
            }
            AppView::CollectionDetail { .. } => {
                self.collection_detail
                    .set_filter(self.search.value.clone());
            }
            _ => {}
        }
    }

    pub fn switch_view(&mut self, view: AppView) {
        match &view {
            AppView::Library => {
                self.library.load(&self.conn, None).ok();
            }
            AppView::Collections => {
                self.collections.load(&self.conn, None).ok();
            }
            AppView::AlbumDetail { album_id } => {
                self.album_detail.load(&self.conn, *album_id).ok();
            }
            AppView::LooseTracks => {
                self.album_detail.load_loose(&self.conn).ok();
            }
            AppView::CollectionDetail { collection_id } => {
                self.collection_detail
                    .load(&self.conn, *collection_id, &self.settings.library.music_dir)
                    .ok();
            }
            AppView::Import => {
                self.import.start(
                    &self.settings.library.inbox_dirs,
                    &self.conn,
                    self.settings.musicbrainz.rate_limit_ms,
                );
            }
            AppView::Editor { track_id } => {
                self.editor.load(&self.conn, *track_id).ok();
            }
            AppView::Help => {
                self.help.visible = true;
            }
        }
        self.search.clear();
        self.search.focused = false;
        self.view = view;
    }

    fn handle_library_key(&mut self, key: KeyEvent) -> AppAction {
        // If the library has an open popup (e.g. add-to-collection), route to it first
        if !self.library.has_popup() {
            if keys::is_tab_switch(&key) || key.code == KeyCode::Char('c') {
                self.switch_view(AppView::Collections);
                return AppAction::None;
            }
            if key.code == KeyCode::Char('i') {
                self.switch_view(AppView::Import);
                return AppAction::None;
            }
        }
        let action = self.library.handle_key(key, &self.conn);
        self.process_library_action(action)
    }

    fn process_library_action(
        &mut self,
        action: super::views::library::LibraryAction,
    ) -> AppAction {
        match action {
            super::views::library::LibraryAction::None => AppAction::None,
            super::views::library::LibraryAction::OpenAlbum(id) => {
                self.switch_view(AppView::AlbumDetail { album_id: id });
                AppAction::None
            }
            super::views::library::LibraryAction::OpenLoose => {
                self.switch_view(AppView::LooseTracks);
                AppAction::None
            }
            super::views::library::LibraryAction::SortChanged => {
                let query = if self.search.value.is_empty() {
                    None
                } else {
                    Some(self.search.value.as_str())
                };
                self.library.load(&self.conn, query).ok();
                AppAction::None
            }
            super::views::library::LibraryAction::OrganizeAll => {
                if self.library.organize_plan.is_some() {
                    // Plan is already showing — Enter was pressed, so apply it
                    if let Some(plan) = self.library.organize_plan.take() {
                        match crate::core::organizer::apply_organize(
                            &self.conn,
                            &plan,
                            &self.settings.import.organize_operation,
                        ) {
                            Ok(result) => {
                                let mut parts = Vec::new();
                                if result.moved > 0 {
                                    parts.push(format!("{} moved", result.moved));
                                }
                                if result.copied > 0 {
                                    parts.push(format!("{} copied", result.copied));
                                }
                                if result.dirs_cleaned > 0 {
                                    parts.push(format!("{} dirs cleaned", result.dirs_cleaned));
                                }
                                if result.orphans_cleaned > 0 {
                                    parts.push(format!(
                                        "{} orphans pruned",
                                        result.orphans_cleaned
                                    ));
                                }
                                if !result.errors.is_empty() {
                                    parts.push(format!("{} errors", result.errors.len()));
                                }
                                self.library.notice =
                                    Some(format!("Organized: {}", parts.join(", ")));
                            }
                            Err(e) => {
                                self.library.notice =
                                    Some(format!("Organize failed: {}", e));
                            }
                        }
                        self.library.load(&self.conn, None).ok();
                        self.refresh_counts();
                    }
                } else {
                    // Compute the plan and show it
                    match crate::core::organizer::plan_organize(
                        &self.conn,
                        &self.settings,
                        crate::core::organizer::OrganizeFilter::All,
                    ) {
                        Ok(plan) => {
                            self.library.organize_plan = Some(plan);
                            self.library.organize_details = false;
                            self.library.organize_scroll = 0;
                        }
                        Err(e) => {
                            self.library.notice =
                                Some(format!("Organize plan failed: {}", e));
                        }
                    }
                }
                AppAction::None
            }
        }
    }

    fn handle_collections_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_tab_switch(&key) {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        let action =
            self.collections
                .handle_key(key, &self.conn, &self.settings.library.music_dir);
        match action {
            super::views::collections::CollectionsAction::None => AppAction::None,
            super::views::collections::CollectionsAction::OpenCollection(id) => {
                self.switch_view(AppView::CollectionDetail { collection_id: id });
                AppAction::None
            }
            super::views::collections::CollectionsAction::Refresh => {
                self.collections.load(&self.conn, None).ok();
                AppAction::None
            }
            super::views::collections::CollectionsAction::SwitchToLibrary => {
                self.switch_view(AppView::Library);
                AppAction::None
            }
            super::views::collections::CollectionsAction::OrganizeAll => {
                if self.collections.organize_plan.is_some() {
                    // Plan showing — Enter applies
                    if let Some(plan) = self.collections.organize_plan.take() {
                        match crate::core::organizer::apply_organize(
                            &self.conn,
                            &plan,
                            &self.settings.import.organize_operation,
                        ) {
                            Ok(_result) => {}
                            Err(_) => {}
                        }
                        self.collections.load(&self.conn, None).ok();
                        self.refresh_counts();
                    }
                } else {
                    // Compute and show
                    match crate::core::organizer::plan_organize(
                        &self.conn,
                        &self.settings,
                        crate::core::organizer::OrganizeFilter::All,
                    ) {
                        Ok(plan) => {
                            self.collections.organize_plan = Some(plan);
                        }
                        Err(_) => {}
                    }
                }
                AppAction::None
            }
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.album_detail.has_popup() {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        let action = self.album_detail.handle_key(key, &self.conn, &self.settings);
        match action {
            super::views::detail::DetailAction::None => AppAction::None,
            super::views::detail::DetailAction::EditTrack(id) => {
                self.editor_return_to = Some(self.view.clone());
                self.switch_view(AppView::Editor { track_id: id });
                AppAction::None
            }
            super::views::detail::DetailAction::Organize => {
                // Compute organize plan for the current album (or loose tracks)
                let filter = if let Some(album) = &self.album_detail.album {
                    crate::core::organizer::OrganizeFilter::AlbumId(album.id)
                } else {
                    crate::core::organizer::OrganizeFilter::Loose
                };
                match crate::core::organizer::plan_organize(
                    &self.conn,
                    &self.settings,
                    filter,
                ) {
                    Ok(plan) => {
                        self.album_detail.set_organize_plan(plan);
                    }
                    Err(e) => {
                        self.album_detail
                            .set_notice(format!("Organize plan failed: {}", e));
                    }
                }
                AppAction::None
            }
        }
    }

    fn handle_collection_detail_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.collection_detail.has_popup() {
            self.switch_view(AppView::Collections);
            return AppAction::None;
        }
        let action = self.collection_detail.handle_key(
            key,
            &self.conn,
            &self.settings.library.music_dir,
        );
        match action {
            super::views::collections::CollectionDetailAction::None => AppAction::None,
            super::views::collections::CollectionDetailAction::EditTrack(id) => {
                self.editor_return_to = Some(self.view.clone());
                self.switch_view(AppView::Editor { track_id: id });
                AppAction::None
            }
            super::views::collections::CollectionDetailAction::Refresh => {
                if let AppView::CollectionDetail { collection_id } = self.view {
                    self.collection_detail
                        .load(
                            &self.conn,
                            collection_id,
                            &self.settings.library.music_dir,
                        )
                        .ok();
                }
                AppAction::None
            }
            super::views::collections::CollectionDetailAction::OpenDir => {
                let visible = self.collection_detail.filtered_indices();
                let track = visible
                    .get(self.collection_detail.selected)
                    .and_then(|&i| self.collection_detail.tracks.get(i));
                if let Some(track) = track {
                    let file_path = self
                        .collection_detail
                        .collection_paths
                        .get(&track.id)
                        .cloned()
                        .unwrap_or_else(|| track.file_path.clone());
                    let path = std::path::Path::new(&file_path);
                    if let Some(parent) = path.parent() {
                        if !parent.exists() {
                            self.collection_detail.notice =
                                Some(format!("Directory not found: {}", parent.display()));
                        } else if !super::views::detail::open_directory(parent) {
                            self.collection_detail.notice = Some(format!(
                                "Could not open file manager — path: {}",
                                parent.display()
                            ));
                        }
                    }
                }
                AppAction::None
            }
            super::views::collections::CollectionDetailAction::Organize => {
                if let Some(coll) = &self.collection_detail.collection {
                    let coll_name = coll.name.clone();

                    if self.collection_detail.organize_plan.is_some() {
                        // Plan already showing — Enter was pressed, apply it
                        if let Some(plan) = self.collection_detail.organize_plan.take() {
                            match crate::core::organizer::apply_organize(
                                &self.conn,
                                &plan,
                                &self.settings.import.organize_operation,
                            ) {
                                Ok(result) => {
                                    let mut parts = Vec::new();
                                    if result.moved > 0 {
                                        parts.push(format!("{} moved", result.moved));
                                    }
                                    if result.copied > 0 {
                                        parts.push(format!("{} copied", result.copied));
                                    }
                                    if result.dirs_cleaned > 0 {
                                        parts.push(format!(
                                            "{} dirs cleaned",
                                            result.dirs_cleaned
                                        ));
                                    }
                                    if !result.errors.is_empty() {
                                        parts.push(format!(
                                            "{} errors",
                                            result.errors.len()
                                        ));
                                    }
                                    // Stash notice — we can't set it on the view
                                    // directly since we'll reload below
                                    let _notice =
                                        format!("Organized: {}", parts.join(", "));
                                }
                                Err(_) => {}
                            }
                            // Reload to reflect new paths
                            if let AppView::CollectionDetail { collection_id } = self.view {
                                self.collection_detail
                                    .load(
                                        &self.conn,
                                        collection_id,
                                        &self.settings.library.music_dir,
                                    )
                                    .ok();
                            }
                            self.refresh_counts();
                        }
                    } else {
                        // Compute and show the plan
                        match crate::core::organizer::plan_organize(
                            &self.conn,
                            &self.settings,
                            crate::core::organizer::OrganizeFilter::Collection(coll_name),
                        ) {
                            Ok(plan) => {
                                self.collection_detail.organize_plan = Some(plan);
                            }
                            Err(_) => {}
                        }
                    }
                }
                AppAction::None
            }
        }
    }

    fn handle_import_key(&mut self, key: KeyEvent) -> AppAction {
        // On the Complete step: any keypress returns to the library.
        if self.import.is_complete() {
            self.switch_view(AppView::Library);
            self.refresh_counts();
            return AppAction::None;
        }

        // When the wizard is capturing text input (e.g. custom-path field),
        // Esc is handled inside the view (clear input / exit custom mode)
        // — don't cancel the whole wizard unless the view itself decides.
        let capturing = self.import.is_capturing_input();

        if !capturing && keys::is_back(&key) && self.import.can_cancel() {
            self.switch_view(AppView::Library);
            self.refresh_counts();
            return AppAction::None;
        }
        self.import.handle_key(key, &self.conn);

        // If the view cleared its capturing state in response to Esc on an
        // empty input, treat that as a wizard cancel.
        if capturing && keys::is_back(&key) && !self.import.is_capturing_input() {
            self.switch_view(AppView::Library);
            self.refresh_counts();
            return AppAction::None;
        }
        AppAction::None
    }

    fn refresh_counts(&mut self) {
        self.track_count = queries::count_tracks(&self.conn).unwrap_or(0);
        self.inbox_count =
            crate::core::importer::scan_inbox(&self.conn, &self.settings.library.inbox_dirs)
                .map(|v| v.len())
                .unwrap_or(0);
    }

    /// Full refresh: reloads counts and the current view's data from disk/DB.
    /// Useful when files are added to the inbox while the TUI is running.
    fn refresh(&mut self) {
        self.refresh_counts();

        let search_query = if self.search.value.is_empty() {
            None
        } else {
            Some(self.search.value.clone())
        };

        match &self.view {
            AppView::Library => {
                self.library
                    .load(&self.conn, search_query.as_deref())
                    .ok();
            }
            AppView::Collections => {
                self.collections
                    .load(&self.conn, search_query.as_deref())
                    .ok();
            }
            AppView::AlbumDetail { album_id } => {
                let prev = self.album_detail.selected;
                self.album_detail.load(&self.conn, *album_id).ok();
                if prev < self.album_detail.tracks.len() {
                    self.album_detail.selected = prev;
                }
            }
            AppView::LooseTracks => {
                let prev = self.album_detail.selected;
                self.album_detail.load_loose(&self.conn).ok();
                if prev < self.album_detail.tracks.len() {
                    self.album_detail.selected = prev;
                }
            }
            AppView::CollectionDetail { collection_id } => {
                let prev = self.collection_detail.selected;
                self.collection_detail
                    .load(&self.conn, *collection_id, &self.settings.library.music_dir)
                    .ok();
                if prev < self.collection_detail.tracks.len() {
                    self.collection_detail.selected = prev;
                }
            }
            _ => {}
        }
    }

    fn handle_global_search_action(&mut self, action: GlobalSearchAction) -> AppAction {
        match action {
            GlobalSearchAction::None => AppAction::None,
            GlobalSearchAction::Close => {
                self.global_search_open = false;
                AppAction::None
            }
            GlobalSearchAction::OpenAlbum(id) => {
                self.global_search_open = false;
                self.switch_view(AppView::AlbumDetail { album_id: id });
                AppAction::None
            }
            GlobalSearchAction::OpenCollection(id) => {
                self.global_search_open = false;
                self.switch_view(AppView::CollectionDetail { collection_id: id });
                AppAction::None
            }
            GlobalSearchAction::OpenTrackAlbum { track_id, album_id } => {
                self.global_search_open = false;
                self.switch_view(AppView::AlbumDetail { album_id });
                // Try to move cursor to that track
                if let Some(pos) = self
                    .album_detail
                    .tracks
                    .iter()
                    .position(|t| t.id == track_id)
                {
                    self.album_detail.selected = pos;
                }
                AppAction::None
            }
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.editor.is_editing() {
            // Return to the view we came from without reloading
            // (preserves cursor position in album/collection detail)
            let return_view = self.editor_return_to.take().unwrap_or(AppView::Library);

            // If the editor made changes, refresh the underlying view's data
            // but keep the cursor in place by re-running load (which clamps selection).
            match &return_view {
                AppView::AlbumDetail { album_id } => {
                    let prev_selected = self.album_detail.selected;
                    self.album_detail.load(&self.conn, *album_id).ok();
                    if prev_selected < self.album_detail.tracks.len() {
                        self.album_detail.selected = prev_selected;
                    }
                }
                AppView::LooseTracks => {
                    let prev_selected = self.album_detail.selected;
                    self.album_detail.load_loose(&self.conn).ok();
                    if prev_selected < self.album_detail.tracks.len() {
                        self.album_detail.selected = prev_selected;
                    }
                }
                AppView::CollectionDetail { collection_id } => {
                    let prev_selected = self.collection_detail.selected;
                    self.collection_detail
                        .load(&self.conn, *collection_id, &self.settings.library.music_dir)
                        .ok();
                    if prev_selected < self.collection_detail.tracks.len() {
                        self.collection_detail.selected = prev_selected;
                    }
                }
                _ => {}
            }

            self.view = return_view;
            return AppAction::None;
        }
        self.editor.handle_key(key, &self.conn);
        AppAction::None
    }

    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        // Clear background
        let bg_block = Block::default().style(Style::default().bg(self.theme.bg));
        frame.render_widget(bg_block, area);

        match &self.view {
            AppView::Library => self.render_library(frame, area),
            AppView::Collections => self.render_collections(frame, area),
            AppView::AlbumDetail { .. } | AppView::LooseTracks => {
                self.render_detail(frame, area)
            }
            AppView::CollectionDetail { .. } => self.render_collection_detail(frame, area),
            AppView::Import => self.render_import(frame, area),
            AppView::Editor { .. } => self.render_editor(frame, area),
            AppView::Help => {
                // Render library underneath, then help overlay
                self.render_library(frame, area);
            }
        }

        // Global search overlay
        if self.global_search_open {
            self.render_global_search(frame, area);
        }

        // Help overlay on top of everything
        if self.help.visible {
            self.render_help_overlay(frame, area);
        }
    }

    fn render_global_search(&self, frame: &mut Frame, area: Rect) {
        // Centered overlay: ~70% width, 60% height
        let height = (area.height * 6 / 10).max(10).min(area.height);
        let width = (area.width * 7 / 10).max(40).min(area.width);
        let x = (area.width - width) / 2;
        let y = (area.height - height) / 2;
        let rect = Rect::new(area.x + x, area.y + y, width, height);
        frame.render_widget(ratatui::widgets::Clear, rect);
        self.global_search.render(frame, rect, self.theme);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let title = Span::styled(
            " kyoku ",
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD),
        );

        let inbox = if self.inbox_count > 0 {
            Span::styled(
                format!("Inbox: {} new ", self.inbox_count),
                Style::default().fg(self.theme.orange),
            )
        } else {
            Span::styled("", Style::default())
        };

        let track_info = Span::styled(
            format!(" Library: {} tracks ", self.track_count),
            Style::default().fg(self.theme.fg_dim),
        );

        let header = Line::from(vec![title, Span::raw(" "), inbox, track_info]);
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(self.theme.border));
        let p = Paragraph::new(header).block(block);
        frame.render_widget(p, area);
    }

    fn render_search_bar(&self, frame: &mut Frame, area: Rect) {
        let tab_indicator = match self.view {
            AppView::Library => "[Albums > Collections]",
            AppView::Collections => "[Albums < Collections]",
            _ => "",
        };

        let search_width = area.width.saturating_sub(tab_indicator.len() as u16 + 1);

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(search_width),
                Constraint::Min(0),
            ])
            .split(area);

        self.search.render(frame, chunks[0], self.theme);

        let tab = Paragraph::new(Span::styled(
            tab_indicator,
            Style::default().fg(self.theme.fg_muted),
        ));
        frame.render_widget(tab, chunks[1]);
    }

    fn render_library(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // table
                Constraint::Length(2), // detail bar
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_search_bar(frame, chunks[1]);
        self.library.render(frame, chunks[2], self.theme);
        self.library.render_detail_bar(frame, chunks[3], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[4],
            self.theme,
            &[
                ("j/k", "nav"),
                ("Enter", "detail"),
                ("i", "import"),
                ("O", "organize"),
                ("/", "filter"),
                ("g", "search"),
                ("a", "add to coll"),
                ("s", "sort"),
                ("F5", "refresh"),
                ("Tab", "colls"),
                ("q", "quit"),
            ],
        );
    }

    fn render_collections(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // table
                Constraint::Length(2), // detail bar
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_search_bar(frame, chunks[1]);
        self.collections.render(frame, chunks[2], self.theme);
        self.collections
            .render_detail_bar(frame, chunks[3], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[4],
            self.theme,
            &[
                ("j/k", "nav"),
                ("Enter", "browse"),
                ("n", "new"),
                ("R", "rename"),
                ("O", "organize all"),
                ("d", "delete"),
                ("Tab", "albums"),
                ("q", "quit"),
            ],
        );
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.search.render(frame, chunks[0], self.theme);
        self.album_detail.render(frame, chunks[1], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[2],
            self.theme,
            #[cfg(not(target_os = "windows"))]
            &[
                ("j/k", "nav"),
                ("/", "filter"),
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("a", "add to coll"),
                ("o", "open dir"),
                ("Esc", "back"),
            ],
            #[cfg(target_os = "windows")]
            &[
                ("j/k", "nav"),
                ("/", "filter"),
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("a", "add to coll"),
                ("Esc", "back"),
            ],
        );
    }

    fn render_collection_detail(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.search.render(frame, chunks[0], self.theme);
        self.collection_detail
            .render(frame, chunks[1], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[2],
            self.theme,
            #[cfg(not(target_os = "windows"))]
            &[
                ("j/k", "nav"),
                ("/", "filter"),
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("o", "open dir"),
                ("x", "remove"),
                ("Esc", "back"),
            ],
            #[cfg(target_os = "windows")]
            &[
                ("j/k", "nav"),
                ("/", "filter"),
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("x", "remove"),
                ("Esc", "back"),
            ],
        );
    }

    fn render_import(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.import.render(frame, chunks[0], self.theme);

        let hints = self.import.status_hints();
        super::widgets::status_bar::render(frame, chunks[1], self.theme, &hints);
    }

    fn render_editor(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.editor.render(frame, chunks[0], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[1],
            self.theme,
            &[
                ("Enter", "edit field"),
                ("Tab", "next"),
                ("Ctrl+S", "save"),
                ("Esc", "cancel"),
            ],
        );
    }

    fn render_help_overlay(&self, frame: &mut Frame, area: Rect) {
        self.help.render(frame, area, self.theme);
    }
}
