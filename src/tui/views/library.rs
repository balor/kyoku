use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use unicode_width::UnicodeWidthStr;

use crate::db::queries::{self, AlbumRow, AlbumSort};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::add_to_collection::{AddToCollectionPopup, PopupAction};

pub enum LibraryAction {
    None,
    OpenAlbum(i64),
    SortChanged,
}

pub struct LibraryView {
    pub albums: Vec<AlbumRow>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub total_albums: usize,
    pub loose_count: i64,
    pub sort: AlbumSort,
    pub sort_ascending: bool,
    pub add_to_collection: Option<AddToCollectionPopup>,
    pub notice: Option<String>,
}

impl Default for LibraryView {
    fn default() -> Self {
        Self {
            albums: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            total_albums: 0,
            loose_count: 0,
            sort: AlbumSort::Artist,
            sort_ascending: true,
            add_to_collection: None,
            notice: None,
        }
    }
}

impl LibraryView {
    pub fn load(&mut self, conn: &Connection, search: Option<&str>) -> Result<()> {
        if let Some(query) = search {
            if !query.is_empty() {
                self.albums = queries::search_albums(conn, query, 500)?;
                self.total_albums = self.albums.len();
                self.loose_count = 0;
                self.clamp_selection();
                return Ok(());
            }
        }
        self.albums = queries::list_albums(conn, self.sort, self.sort_ascending, 0, 500)?;
        self.total_albums = self.albums.len();
        self.loose_count = queries::count_loose_tracks(conn)?;
        self.clamp_selection();
        Ok(())
    }

    fn item_count(&self) -> usize {
        let base = self.albums.len();
        if self.loose_count > 0 {
            base + 1
        } else {
            base
        }
    }

    fn clamp_selection(&mut self) {
        let count = self.item_count();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    pub fn has_popup(&self) -> bool {
        self.add_to_collection.is_some()
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) -> LibraryAction {
        // Popup captures input
        if let Some(popup) = &mut self.add_to_collection {
            match popup.handle_key(key, conn) {
                PopupAction::None => {}
                PopupAction::Closed(notice) => {
                    self.add_to_collection = None;
                    if let Some(n) = notice {
                        if !n.is_empty() {
                            self.notice = Some(n);
                        }
                    }
                }
            }
            return LibraryAction::None;
        }

        let count = self.item_count();

        if keys::is_up(&key) {
            if self.selected > 0 {
                self.selected -= 1;
            }
            return LibraryAction::None;
        }

        if keys::is_down(&key) {
            if count > 0 && self.selected < count - 1 {
                self.selected += 1;
            }
            return LibraryAction::None;
        }

        if keys::is_page_up(&key) {
            self.selected = self.selected.saturating_sub(20);
            return LibraryAction::None;
        }

        if keys::is_page_down(&key) {
            if count > 0 {
                self.selected = (self.selected + 20).min(count - 1);
            }
            return LibraryAction::None;
        }

        if keys::is_half_page_up(&key) {
            self.selected = self.selected.saturating_sub(10);
            return LibraryAction::None;
        }

        if keys::is_half_page_down(&key) {
            if count > 0 {
                self.selected = (self.selected + 10).min(count - 1);
            }
            return LibraryAction::None;
        }

        if keys::is_confirm(&key) {
            if self.selected < self.albums.len() {
                return LibraryAction::OpenAlbum(self.albums[self.selected].id);
            }
            // [loose] entry — could navigate to loose tracks view later
            return LibraryAction::None;
        }

        if key.code == KeyCode::Char('s') {
            self.sort = self.sort.next();
            return LibraryAction::SortChanged;
        }

        if key.code == KeyCode::Char('G') {
            if count > 0 {
                self.selected = count - 1;
            }
            return LibraryAction::None;
        }

        if key.code == KeyCode::Char('a') {
            // Add whole album (all tracks of selected album) to a collection
            if self.selected < self.albums.len() {
                let album = &self.albums[self.selected];
                if let Ok(tracks) = queries::get_album_tracks(conn, album.id) {
                    if !tracks.is_empty() {
                        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
                        let display =
                            format!("{} ({} tracks)", album.title, tracks.len());
                        self.add_to_collection =
                            Some(AddToCollectionPopup::open(ids, display, conn));
                    }
                }
            }
            return LibraryAction::None;
        }

        LibraryAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let visible_height = area.height.saturating_sub(1) as usize; // -1 for header

        // Adjust scroll offset — keep 1 extra row visible below cursor for lookahead
        let scroll_offset = if self.selected < self.scroll_offset {
            self.selected
        } else if self.selected + 1 >= self.scroll_offset + visible_height {
            (self.selected + 2).saturating_sub(visible_height)
        } else {
            self.scroll_offset
        };

        // Build rows
        let mut rows: Vec<Row> = Vec::new();
        let count = self.item_count();

        for i in scroll_offset..count.min(scroll_offset + visible_height) {
            let is_selected = i == self.selected;

            let row = if i < self.albums.len() {
                let album = &self.albums[i];
                let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
                let year = album
                    .year
                    .map(|y| y.to_string())
                    .unwrap_or_default();
                let fmt = abbreviate_formats(&album.formats);

                Row::new(vec![
                    Cell::from(truncate_str(artist, 24)),
                    Cell::from(truncate_str(&album.title, 30)),
                    Cell::from(year),
                    Cell::from(format!("{:>4}", album.track_count)),
                    Cell::from(fmt),
                ])
            } else {
                // [loose] entry
                Row::new(vec![
                    Cell::from(Span::styled(
                        "[loose]",
                        Style::default().fg(theme.fg_muted),
                    )),
                    Cell::from(Span::styled(
                        format!("{} unalbumed tracks", self.loose_count),
                        Style::default().fg(theme.fg_muted),
                    )),
                    Cell::from(""),
                    Cell::from(format!("{:>4}", self.loose_count)),
                    Cell::from("mix"),
                ])
            };

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

        let header = Row::new(vec![
            Cell::from(Span::styled("Artist", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Album", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Year", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Tracks", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Fmt", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(0);

        let sort_indicator = format!(" [sort: {}] ", self.sort.label());

        let table = Table::new(
            rows,
            [
                ratatui::layout::Constraint::Percentage(25),
                ratatui::layout::Constraint::Percentage(30),
                ratatui::layout::Constraint::Length(6),
                ratatui::layout::Constraint::Length(6),
                ratatui::layout::Constraint::Length(6),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(theme.border))
                .title_bottom(Span::styled(
                    sort_indicator,
                    Style::default().fg(theme.fg_muted),
                )),
        );

        frame.render_widget(table, area);

        // Add-to-collection popup overlay
        if let Some(popup) = &self.add_to_collection {
            popup.render(frame, area, theme);
        }
    }

    pub fn render_detail_bar(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if let Some(notice) = &self.notice {
            let p = Paragraph::new(Span::styled(
                format!(" {} ", notice),
                Style::default().fg(theme.green),
            ))
            .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
            return;
        }
        if self.selected < self.albums.len() {
            let album = &self.albums[self.selected];
            let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
            let year = album
                .year
                .map(|y| format!(" ({})", y))
                .unwrap_or_default();
            let fmt = abbreviate_formats(&album.formats);
            let duration = format_duration_ms(album.total_duration_ms);

            let detail = format!(
                " {} — {}{} · {} · {} tracks · {}",
                artist, album.title, year, fmt, album.track_count, duration
            );
            let p = Paragraph::new(Span::styled(detail, Style::default().fg(theme.fg_dim)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
        } else if self.loose_count > 0 {
            let detail = format!(" {} loose tracks (no album)", self.loose_count);
            let p = Paragraph::new(Span::styled(detail, Style::default().fg(theme.fg_muted)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
        }
    }
}

fn truncate_str(s: &str, max_width: usize) -> String {
    let width = s.width();
    if width <= max_width {
        s.to_string()
    } else {
        let mut result = String::new();
        let mut w = 0;
        for c in s.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if w + cw > max_width.saturating_sub(1) {
                result.push('…');
                break;
            }
            result.push(c);
            w += cw;
        }
        result
    }
}

fn abbreviate_formats(formats: &str) -> String {
    let fmts: Vec<&str> = formats.split(',').collect();
    if fmts.len() == 1 {
        fmts[0].to_uppercase()
    } else {
        "mix".to_string()
    }
}

pub fn format_duration_ms(ms: i64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{}h {:02}m", hours, mins)
    } else {
        format!("{}:{:02}", mins, secs)
    }
}
