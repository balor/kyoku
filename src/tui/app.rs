//! Top-level `App` state and entry points. The detailed handling lives in
//! sibling submodules — `handlers` owns key/action processing and navigation,
//! `render` owns every draw call.

mod handlers;
mod render;

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use rusqlite::Connection;

use crate::config::Settings;
use crate::db::queries;

use super::keybindings as keys;
use super::themes::Theme;
use super::views::collections::{CollectionDetailView, CollectionsView};
use super::views::detail::AlbumDetailView;
use super::views::edit::EditorView;
use super::views::global_search::GlobalSearchView;
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
}

pub enum AppAction {
    None,
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

            search: TextInput::new("press / to filter, g for global search"),
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
        }
    }

    pub fn tick(&mut self) {
        // Check debounced search
        if let Some(deadline) = self.search_debounce
            && Instant::now() >= deadline {
                self.on_search_changed();
                self.search_debounce = None;
            }

        // Tick import view for background operations
        if self.view == AppView::Import {
            self.import.tick(&self.conn);
        }
    }
}
