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
use super::views::help::HelpOverlay;
use super::views::import::ImportView;
use super::views::library::LibraryView;
use super::widgets::input::TextInput;

#[derive(Debug, Clone, PartialEq)]
pub enum AppView {
    Library,
    Collections,
    AlbumDetail { album_id: i64 },
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

    // Search
    pub search: TextInput,
    pub search_debounce: Option<Instant>,
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

            search: TextInput::new("Search..."),
            search_debounce: None,
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

        // Global keys (always active unless search is focused)
        if !self.search.focused {
            if keys::is_quit(&key) {
                return AppAction::Quit;
            }
            if keys::is_help(&key) {
                self.help.visible = true;
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

        if keys::is_search_focus(&key) {
            self.search.focused = true;
            return AppAction::None;
        }

        // Delegate to current view
        match &self.view {
            AppView::Library => self.handle_library_key(key),
            AppView::Collections => self.handle_collections_key(key),
            AppView::AlbumDetail { .. } => self.handle_detail_key(key),
            AppView::CollectionDetail { .. } => self.handle_collection_detail_key(key),
            AppView::Import => self.handle_import_key(key),
            AppView::Editor { .. } => self.handle_editor_key(key),
            AppView::Help => AppAction::None,
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
            AppView::CollectionDetail { collection_id } => {
                self.collection_detail
                    .load(&self.conn, *collection_id)
                    .ok();
            }
            AppView::Import => {
                self.import
                    .start(&self.settings.library.inbox_dirs, &self.conn);
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
        if keys::is_tab_switch(&key) || key.code == KeyCode::Char('c') {
            self.switch_view(AppView::Collections);
            return AppAction::None;
        }
        if key.code == KeyCode::Char('i') {
            self.switch_view(AppView::Import);
            return AppAction::None;
        }
        let action = self.library.handle_key(key);
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
            super::views::library::LibraryAction::SortChanged => {
                let query = if self.search.value.is_empty() {
                    None
                } else {
                    Some(self.search.value.as_str())
                };
                self.library.load(&self.conn, query).ok();
                AppAction::None
            }
        }
    }

    fn handle_collections_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_tab_switch(&key) {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        let action = self.collections.handle_key(key, &self.conn);
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
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.album_detail.is_renaming() {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        let action = self.album_detail.handle_key(key, &self.conn);
        match action {
            super::views::detail::DetailAction::None => AppAction::None,
            super::views::detail::DetailAction::EditTrack(id) => {
                self.switch_view(AppView::Editor { track_id: id });
                AppAction::None
            }
            super::views::detail::DetailAction::AddToCollection(track_id) => {
                // Add to collection via popup - for now just show in status
                let _ = track_id;
                AppAction::None
            }
        }
    }

    fn handle_collection_detail_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) {
            self.switch_view(AppView::Collections);
            return AppAction::None;
        }
        let action = self.collection_detail.handle_key(key, &self.conn);
        match action {
            super::views::collections::CollectionDetailAction::None => AppAction::None,
            super::views::collections::CollectionDetailAction::EditTrack(id) => {
                self.switch_view(AppView::Editor { track_id: id });
                AppAction::None
            }
            super::views::collections::CollectionDetailAction::Refresh => {
                if let AppView::CollectionDetail { collection_id } = self.view {
                    self.collection_detail.load(&self.conn, collection_id).ok();
                }
                AppAction::None
            }
        }
    }

    fn handle_import_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && self.import.can_cancel() {
            self.switch_view(AppView::Library);
            self.track_count = queries::count_tracks(&self.conn).unwrap_or(0);
            return AppAction::None;
        }
        self.import.handle_key(key, &self.conn);
        if self.import.is_complete() && keys::is_confirm(&key) {
            self.switch_view(AppView::Library);
            self.track_count = queries::count_tracks(&self.conn).unwrap_or(0);
        }
        AppAction::None
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.editor.is_editing() {
            // Go back to previous view
            self.switch_view(AppView::Library);
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
            AppView::AlbumDetail { .. } => self.render_detail(frame, area),
            AppView::CollectionDetail { .. } => self.render_collection_detail(frame, area),
            AppView::Import => self.render_import(frame, area),
            AppView::Editor { .. } => self.render_editor(frame, area),
            AppView::Help => {
                // Render library underneath, then help overlay
                self.render_library(frame, area);
            }
        }

        // Help overlay on top of everything
        if self.help.visible {
            self.render_help_overlay(frame, area);
        }
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
                ("/", "search"),
                ("s", "sort"),
                ("Tab", "collections"),
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
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.album_detail.render(frame, chunks[0], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[1],
            self.theme,
            &[
                ("j/k", "nav"),
                ("e", "edit"),
                ("R", "rename album"),
                ("a", "add to coll"),
                ("o", "open dir"),
                ("Esc", "back"),
            ],
        );
    }

    fn render_collection_detail(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.collection_detail
            .render(frame, chunks[0], self.theme);
        super::widgets::status_bar::render(
            frame,
            chunks[1],
            self.theme,
            &[
                ("j/k", "nav"),
                ("e", "edit"),
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
