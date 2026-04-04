use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::db::queries::{self, AlbumRow, TrackRow};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::views::library::format_duration_ms;
use crate::tui::widgets::input::TextInput;

pub enum DetailAction {
    None,
    EditTrack(i64),
    AddToCollection(i64),
}

#[derive(Default)]
pub struct AlbumDetailView {
    pub album: Option<AlbumRow>,
    pub tracks: Vec<TrackRow>,
    pub filter: String,
    pub selected: usize,
    pub scroll_offset: usize,
    rename_input: Option<TextInput>,
}

impl AlbumDetailView {
    /// Return indices of tracks matching the current filter.
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

impl AlbumDetailView {
    pub fn load(&mut self, conn: &Connection, album_id: i64) -> Result<()> {
        self.album = queries::get_album(conn, album_id)?;
        self.tracks = queries::get_album_tracks(conn, album_id)?;
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.rename_input = None;
        Ok(())
    }

    pub fn is_renaming(&self) -> bool {
        self.rename_input.is_some()
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) -> DetailAction {
        // Rename mode captures input
        if let Some(input) = &mut self.rename_input {
            if keys::is_back(&key) {
                self.rename_input = None;
                return DetailAction::None;
            }
            if keys::is_confirm(&key) {
                let new_title = input.value.trim().to_string();
                if let Some(album) = &self.album {
                    if !new_title.is_empty() && new_title != album.title {
                        let id = album.id;
                        queries::rename_album(conn, id, &new_title).ok();
                        // Reload to reflect the change
                        self.load(conn, id).ok();
                        return DetailAction::None;
                    }
                }
                self.rename_input = None;
                return DetailAction::None;
            }
            input.handle_key(key);
            return DetailAction::None;
        }

        let visible = self.filtered_indices();
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
        if key.code == KeyCode::Char('g') {
            self.selected = 0;
        }
        if key.code == KeyCode::Char('G') && count > 0 {
            self.selected = count - 1;
        }

        let current_track = visible
            .get(self.selected)
            .and_then(|&i| self.tracks.get(i));

        if key.code == KeyCode::Char('e') {
            if let Some(track) = current_track {
                return DetailAction::EditTrack(track.id);
            }
        }
        if key.code == KeyCode::Char('a') {
            if let Some(track) = current_track {
                return DetailAction::AddToCollection(track.id);
            }
        }
        if key.code == KeyCode::Char('o') {
            if let Some(track) = current_track {
                if let Some(parent) = std::path::Path::new(&track.file_path).parent() {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(parent)
                        .spawn();
                }
            }
        }
        if key.code == KeyCode::Char('R') {
            // Rename album
            if let Some(album) = &self.album {
                let mut input =
                    TextInput::new("New album title...").with_label(" Title: ");
                input.value = album.title.clone();
                input.cursor = input.value.len();
                input.focused = true;
                self.rename_input = Some(input);
            }
        }

        DetailAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // album header
                Constraint::Min(5),   // track table
                Constraint::Length(2), // metadata
            ])
            .split(area);

        // Album header
        if let Some(album) = &self.album {
            let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
            let year = album
                .year
                .map(|y| format!(" ({})", y))
                .unwrap_or_default();
            let header = Line::from(vec![
                Span::styled(
                    format!(" {} ", artist),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("— ", Style::default().fg(theme.fg_muted)),
                Span::styled(
                    format!("{}{}", album.title, year),
                    Style::default()
                        .fg(theme.fg)
                        .add_modifier(Modifier::BOLD),
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

            let num = track
                .track_number
                .map(|n| format!("{:>2}", n))
                .unwrap_or_else(|| "  ".to_string());
            let duration = track
                .duration_ms
                .map(|ms| {
                    let s = ms / 1000;
                    format!("{}:{:02}", s / 60, s % 60)
                })
                .unwrap_or_default();
            let bitrate = track
                .bitrate
                .map(|b| format!("{} kbps", b))
                .unwrap_or_default();

            let status_style = match track.tag_status.as_str() {
                "verified" => Style::default().fg(theme.green),
                "matched" => Style::default().fg(theme.cyan),
                "manual" => Style::default().fg(theme.yellow),
                _ => Style::default().fg(theme.fg_muted),
            };

            let row = Row::new(vec![
                Cell::from(num),
                Cell::from(track.title.clone()),
                Cell::from(duration),
                Cell::from(Span::styled(track.tag_status.clone(), status_style)),
                Cell::from(bitrate),
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
            Cell::from(Span::styled("#", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Title", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Duration", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Status", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Bitrate", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Percentage(45),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .header(header);

        frame.render_widget(table, chunks[1]);

        // Metadata footer
        if let Some(album) = &self.album {
            let fmt = album.formats.to_uppercase();
            let duration = format_duration_ms(album.total_duration_ms);
            let meta = format!(
                " {} · {} tracks · {}",
                fmt, album.track_count, duration
            );
            let p = Paragraph::new(Span::styled(meta, Style::default().fg(theme.fg_dim)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, chunks[2]);
        }

        // Rename popup
        if let Some(input) = &self.rename_input {
            use crate::tui::widgets::popup;
            let content = vec![Line::from("")];
            let inner = popup::render_popup(frame, area, theme, "Rename Album", &content, 60, 5);
            input.render(frame, inner, theme);
        }
    }
}
