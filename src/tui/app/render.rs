//! Rendering for the top-level `App`. Dispatches to per-view renderers and
//! overlays global chrome (header, search bar, status bars, help).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::{App, AppView};

impl App {
    pub fn render(&mut self, frame: &mut Frame) {
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

    fn render_global_search(&mut self, frame: &mut Frame, area: Rect) {
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

    fn render_library(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // table
                Constraint::Length(1), // detail bar
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_search_bar(frame, chunks[1]);
        self.library.render(frame, chunks[2], self.theme);
        self.library.render_detail_bar(frame, chunks[3], self.theme);
        crate::tui::widgets::status_bar::render(
            frame,
            chunks[4],
            self.theme,
            &[
                ("i", "import"),
                ("O", "organize"),
                ("p", "play"),
                ("a", "add to coll"),
                ("d", "delete"),
                ("s", "sort"),
                ("Tab", "colls"),
                ("?", "keybinds"),
                ("q", "quit"),
            ],
        );
    }

    fn render_collections(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // table
                Constraint::Length(1), // detail bar
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_search_bar(frame, chunks[1]);
        self.collections.render(frame, chunks[2], self.theme);
        self.collections
            .render_detail_bar(frame, chunks[3], self.theme);
        crate::tui::widgets::status_bar::render(
            frame,
            chunks[4],
            self.theme,
            &[
                ("n", "new"),
                ("R", "rename"),
                ("O", "organize all"),
                ("p", "play"),
                ("d", "delete"),
                ("Tab", "albums"),
                ("?", "keybinds"),
                ("q", "quit"),
            ],
        );
    }

    fn render_detail(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // search bar
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.search.render(frame, chunks[0], self.theme);
        self.album_detail.render(
            frame,
            chunks[1],
            self.theme,
            &mut self.covers,
            self.settings.ui.show_cover_preview,
        );
        crate::tui::widgets::status_bar::render(
            frame,
            chunks[2],
            self.theme,
            #[cfg(not(target_os = "windows"))]
            &[
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("C", "fetch cover"),
                ("p", "play"),
                ("P", "play album"),
                ("a", "add to coll"),
                ("d", "delete"),
                ("o", "open dir"),
                ("?", "keybinds"),
            ],
            #[cfg(target_os = "windows")]
            &[
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("C", "fetch cover"),
                ("p", "play"),
                ("P", "play album"),
                ("a", "add to coll"),
                ("d", "delete"),
                ("?", "keybinds"),
            ],
        );
    }

    fn render_collection_detail(&mut self, frame: &mut Frame, area: Rect) {
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
        crate::tui::widgets::status_bar::render(
            frame,
            chunks[2],
            self.theme,
            #[cfg(not(target_os = "windows"))]
            &[
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("p", "play"),
                ("P", "play coll"),
                ("o", "open dir"),
                ("x", "remove"),
                ("d", "delete"),
                ("?", "keybinds"),
            ],
            #[cfg(target_os = "windows")]
            &[
                ("e", "edit"),
                ("R", "rename"),
                ("O", "organize"),
                ("p", "play"),
                ("P", "play coll"),
                ("x", "remove"),
                ("d", "delete"),
                ("?", "keybinds"),
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

        // Cancel-confirmation popup overlays the import view.
        if let Some(state) = &self.import.confirm_cancel {
            state.popup.render(frame, chunks[0], self.theme);
        }

        let hints = self.import.status_hints();
        crate::tui::widgets::status_bar::render(frame, chunks[1], self.theme, &hints);
    }

    fn render_editor(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),   // content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.editor.render(frame, chunks[0], self.theme);
        if let Some(popup) = &self.editor.pending_discard {
            popup.render(frame, chunks[0], self.theme);
        }
        crate::tui::widgets::status_bar::render(
            frame,
            chunks[1],
            self.theme,
            &[
                ("Enter", "edit field"),
                ("Tab", "next"),
                ("Ctrl+S", "save"),
            ],
        );
    }

    fn render_help_overlay(&mut self, frame: &mut Frame, area: Rect) {
        self.help.render(frame, area, self.theme);
    }
}
