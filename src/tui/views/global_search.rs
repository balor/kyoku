use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rusqlite::Connection;

use crate::db::queries::{self, AlbumRow, CollectionRow, TrackRow};
use crate::tui::fuzzy;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::input::TextInput;

#[derive(Debug, Clone)]
pub enum GlobalResult {
    Album(AlbumRow),
    Track(TrackRow),
    Collection(CollectionRow),
}

pub enum GlobalSearchAction {
    None,
    OpenAlbum(i64),
    OpenCollection(i64),
    OpenTrackAlbum { track_id: i64, album_id: i64 },
    Close,
}

pub struct GlobalSearchView {
    pub input: TextInput,
    pub results: Vec<GlobalResult>,
    pub selected: usize,
    pub scroll_offset: usize,
}

impl Default for GlobalSearchView {
    fn default() -> Self {
        let mut input = TextInput::new("Search everything...").with_label(" > ");
        input.focused = true;
        Self {
            input,
            results: Vec::new(),
            selected: 0,
            scroll_offset: 0,
        }
    }
}

impl GlobalSearchView {
    pub fn open(&mut self) {
        self.input = TextInput::new("Search everything...").with_label(" > ");
        self.input.focused = true;
        self.results.clear();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        conn: &Connection,
        music_dir: &std::path::Path,
    ) -> GlobalSearchAction {
        if keys::is_back(&key) {
            return GlobalSearchAction::Close;
        }

        if keys::is_confirm(&key) {
            if let Some(result) = self.results.get(self.selected) {
                return match result {
                    GlobalResult::Album(a) => GlobalSearchAction::OpenAlbum(a.id),
                    GlobalResult::Collection(c) => GlobalSearchAction::OpenCollection(c.id),
                    GlobalResult::Track(t) => {
                        // Look up the track's album; if none, just close
                        if let Ok(Some(album_id)) = get_track_album_id(conn, t.id) {
                            GlobalSearchAction::OpenTrackAlbum {
                                track_id: t.id,
                                album_id,
                            }
                        } else {
                            GlobalSearchAction::Close
                        }
                    }
                };
            }
            return GlobalSearchAction::None;
        }

        if keys::is_up(&key) && self.selected > 0 {
            self.selected -= 1;
            return GlobalSearchAction::None;
        }
        if keys::is_down(&key)
            && !self.results.is_empty()
            && self.selected < self.results.len() - 1
        {
            self.selected += 1;
            return GlobalSearchAction::None;
        }
        if keys::is_page_up(&key) {
            self.selected = self.selected.saturating_sub(10);
            return GlobalSearchAction::None;
        }
        if keys::is_page_down(&key) && !self.results.is_empty() {
            self.selected = (self.selected + 10).min(self.results.len() - 1);
            return GlobalSearchAction::None;
        }

        if self.input.handle_key(key) {
            self.execute(conn, music_dir);
        }
        GlobalSearchAction::None
    }

    pub fn execute(&mut self, conn: &Connection, music_dir: &std::path::Path) {
        self.results.clear();
        self.selected = 0;
        self.scroll_offset = 0;

        let query = self.input.value.trim();
        if query.is_empty() {
            return;
        }

        // Albums (up to 20)
        if let Ok(albums) = queries::search_albums(conn, music_dir, query, 20) {
            for a in albums {
                self.results.push(GlobalResult::Album(a));
            }
        }

        // Tracks (up to 30)
        if let Ok(tracks) = queries::search_tracks(conn, music_dir, query, 30) {
            // Apply client-side fuzzy filter as well for better ranking
            for t in tracks {
                let artist = t.artist.clone().unwrap_or_default();
                if fuzzy::matches_any(query, &[&t.title, &artist]) {
                    self.results.push(GlobalResult::Track(t));
                }
            }
        }

        // Collections (all, then filter)
        if let Ok(colls) = queries::list_collections(conn) {
            for c in colls {
                let desc = c.description.clone().unwrap_or_default();
                if fuzzy::matches_any(query, &[&c.name, &desc]) {
                    self.results.push(GlobalResult::Collection(c));
                }
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(Span::styled(
                " Global Search ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_bright))
            .style(Style::default().bg(theme.bg));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // input
                Constraint::Length(1), // separator/hint
                Constraint::Min(5),   // results
            ])
            .split(inner);

        self.input.render(frame, chunks[0], theme);

        let hint = if self.input.value.is_empty() {
            "Type to search albums, tracks and collections…"
        } else if self.results.is_empty() {
            "No results"
        } else {
            ""
        };
        let p = Paragraph::new(Span::styled(
            format!(" {} ", hint),
            Style::default().fg(theme.fg_muted),
        ));
        frame.render_widget(p, chunks[1]);

        self.render_results(frame, chunks[2], theme);
    }

    fn render_results(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let visible_height = area.height as usize;
        self.scroll_offset = crate::tui::views::library::compute_scroll_offset(
            self.selected,
            self.scroll_offset,
            visible_height,
        );
        let scroll = self.scroll_offset;

        let mut lines: Vec<Line<'_>> = Vec::new();
        for pos in scroll..self.results.len().min(scroll + visible_height) {
            let is_selected = pos == self.selected;
            let line = self.format_result(&self.results[pos], is_selected, theme);
            lines.push(line);
        }

        if self.results.is_empty() {
            return;
        }

        let p = Paragraph::new(lines);
        frame.render_widget(p, area);
    }

    fn format_result<'a>(
        &self,
        result: &'a GlobalResult,
        selected: bool,
        theme: &'a Theme,
    ) -> Line<'a> {
        let prefix_style = if selected {
            Style::default()
                .bg(theme.bg_selected)
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let (kind, kind_color, main, secondary) = match result {
            GlobalResult::Album(a) => (
                "album",
                theme.accent,
                a.title.clone(),
                a.album_artist.clone().unwrap_or_default(),
            ),
            GlobalResult::Track(t) => (
                "track",
                theme.cyan,
                t.title.clone(),
                t.artist.clone().unwrap_or_default(),
            ),
            GlobalResult::Collection(c) => (
                "coll ",
                theme.accent_alt,
                c.name.clone(),
                c.description.clone().unwrap_or_default(),
            ),
        };

        let cursor = if selected { "▶ " } else { "  " };

        Line::from(vec![
            Span::styled(cursor, prefix_style),
            Span::styled(
                format!("[{}] ", kind),
                Style::default()
                    .fg(kind_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                main,
                if selected {
                    Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                },
            ),
            Span::styled(
                if secondary.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", secondary)
                },
                Style::default().fg(theme.fg_dim),
            ),
        ])
    }
}

fn get_track_album_id(conn: &Connection, track_id: i64) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT album_id FROM tracks WHERE id = ?1",
        [track_id],
        |row| row.get::<_, Option<i64>>(0),
    )
}
