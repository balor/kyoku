use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::db::queries;
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::input::TextInput;

pub struct EditField {
    pub name: String,
    pub db_column: String,
    pub value: String,
    pub original: String,
}

impl EditField {
    pub fn is_modified(&self) -> bool {
        self.value != self.original
    }
}

pub struct EditorView {
    pub track_id: i64,
    pub track_title: String,
    pub fields: Vec<EditField>,
    pub selected: usize,
    pub editing: bool,
    pub input: TextInput,
    pub saved: bool,
}

impl Default for EditorView {
    fn default() -> Self {
        Self {
            track_id: 0,
            track_title: String::new(),
            fields: Vec::new(),
            selected: 0,
            editing: false,
            input: TextInput::new(""),
            saved: false,
        }
    }
}

impl EditorView {
    pub fn load(&mut self, conn: &Connection, track_id: i64) -> Result<()> {
        self.track_id = track_id;
        self.selected = 0;
        self.editing = false;
        self.saved = false;

        if let Some(track) = queries::get_track(conn, track_id)? {
            self.track_title = track.title.clone();
            self.fields = vec![
                EditField {
                    name: "Title".to_string(),
                    db_column: "title".to_string(),
                    value: track.title,
                    original: String::new(),
                },
                EditField {
                    name: "Artist".to_string(),
                    db_column: "artist".to_string(),
                    value: track.artist.unwrap_or_default(),
                    original: String::new(),
                },
                EditField {
                    name: "Track Number".to_string(),
                    db_column: "track_number".to_string(),
                    value: track
                        .track_number
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    original: String::new(),
                },
                EditField {
                    name: "Disc Number".to_string(),
                    db_column: "disc_number".to_string(),
                    value: track.disc_number.to_string(),
                    original: String::new(),
                },
            ];
            // Store originals
            for field in &mut self.fields {
                field.original = field.value.clone();
            }
        }
        Ok(())
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) {
        if self.editing {
            if keys::is_back(&key) {
                // Cancel edit, restore value
                if let Some(field) = self.fields.get(self.selected) {
                    self.input.value = field.value.clone();
                }
                self.editing = false;
                return;
            }
            if keys::is_confirm(&key) {
                // Apply edit
                if let Some(field) = self.fields.get_mut(self.selected) {
                    field.value = self.input.value.clone();
                }
                self.editing = false;
                return;
            }
            self.input.handle_key(key);
            return;
        }

        if keys::is_save(&key) {
            self.save(conn);
            return;
        }

        if keys::is_confirm(&key) {
            // Start editing current field
            if let Some(field) = self.fields.get(self.selected) {
                self.input = TextInput::new("");
                self.input.value = field.value.clone();
                self.input.cursor = self.input.value.len();
                self.input.focused = true;
                self.editing = true;
            }
            return;
        }

        if keys::is_up(&key) && self.selected > 0 {
            self.selected -= 1;
        }
        if keys::is_down(&key) && !self.fields.is_empty() && self.selected < self.fields.len() - 1
        {
            self.selected += 1;
        }
        if key.code == KeyCode::Tab && !self.fields.is_empty() {
            self.selected = (self.selected + 1) % self.fields.len();
        }
    }

    fn save(&mut self, conn: &Connection) {
        let modified: Vec<(&str, &str)> = self
            .fields
            .iter()
            .filter(|f| f.is_modified())
            .map(|f| (f.db_column.as_str(), f.value.as_str()))
            .collect();

        if !modified.is_empty() {
            queries::update_track_fields(conn, self.track_id, &modified).ok();
            // Update originals
            for field in &mut self.fields {
                field.original = field.value.clone();
            }
        }
        self.saved = true;
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Min(5),   // fields
                Constraint::Length(2), // notice
            ])
            .split(area);

        // Header
        let header = Line::from(vec![
            Span::styled(
                " Editing: ",
                Style::default().fg(theme.fg_dim),
            ),
            Span::styled(
                &self.track_title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let p = Paragraph::new(header).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme.border)),
        );
        frame.render_widget(p, chunks[0]);

        // Fields table
        let mut rows = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            let is_selected = i == self.selected;
            let is_modified = field.is_modified();

            let value_span = if is_modified {
                Span::styled(&field.value, Style::default().fg(theme.yellow))
            } else {
                Span::styled(&field.value, Style::default().fg(theme.fg))
            };

            let original_span = if is_modified {
                Span::styled(&field.original, Style::default().fg(theme.fg_muted))
            } else {
                Span::styled("", Style::default())
            };

            let row = Row::new(vec![
                Cell::from(Span::styled(&field.name, Style::default().fg(theme.fg_dim))),
                Cell::from(value_span),
                Cell::from(original_span),
            ]);

            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(if i % 2 == 0 {
                    theme.bg
                } else {
                    theme.bg_alt
                })
            };

            rows.push(row.style(style));
        }

        let table_header = Row::new(vec![
            Cell::from(Span::styled("Field", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Value", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Original", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(
            rows,
            [
                Constraint::Length(15),
                Constraint::Percentage(40),
                Constraint::Percentage(40),
            ],
        )
        .header(table_header);

        frame.render_widget(table, chunks[1]);

        // Inline editing overlay
        if self.editing {
            // Render input at the selected field's position
            let input_y = chunks[1].y + 1 + self.selected as u16; // +1 for header
            if input_y < chunks[1].y + chunks[1].height {
                let input_area = Rect::new(
                    chunks[1].x + 16, // after field name column
                    input_y,
                    chunks[1].width.saturating_sub(16),
                    1,
                );
                self.input.render(frame, input_area, theme);
            }
        }

        // Notice
        let notice = if self.saved {
            Span::styled(
                " Saved to database. (File tags not yet written — coming later)",
                Style::default().fg(theme.green),
            )
        } else {
            let has_changes = self.fields.iter().any(|f| f.is_modified());
            if has_changes {
                Span::styled(
                    " Unsaved changes. Press Ctrl+S to save.",
                    Style::default().fg(theme.yellow),
                )
            } else {
                Span::styled(
                    " Press Enter to edit a field.",
                    Style::default().fg(theme.fg_muted),
                )
            }
        };
        let p = Paragraph::new(Line::from(notice)).style(Style::default().bg(theme.bg_alt));
        frame.render_widget(p, chunks[2]);
    }
}
