use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::themes::Theme;

pub struct TextInput {
    pub value: String,
    pub cursor: usize,
    pub focused: bool,
    pub placeholder: String,
    pub label: String,
}

impl TextInput {
    pub fn new(placeholder: &str) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            focused: false,
            placeholder: placeholder.to_string(),
            label: " Search: ".to_string(),
        }
    }

    pub fn with_label(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    /// Handle a key event. Returns true if the value changed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // Find the previous character boundary
                    let prev = self.value[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.value.drain(prev..self.cursor);
                    self.cursor = prev;
                    true
                } else {
                    false
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.value.len() {
                    let next = self.value[self.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor + i)
                        .unwrap_or(self.value.len());
                    self.value.drain(self.cursor..next);
                    true
                } else {
                    false
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.value[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                false
            }
            KeyCode::Right => {
                if self.cursor < self.value.len() {
                    self.cursor = self.value[self.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor + i)
                        .unwrap_or(self.value.len());
                }
                false
            }
            KeyCode::Home => {
                self.cursor = 0;
                false
            }
            KeyCode::End => {
                self.cursor = self.value.len();
                false
            }
            _ => false,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let label = self.label.as_str();
        let label_span = Span::styled(label, Style::default().fg(theme.fg_dim));

        let (text_span, cursor_visible) = if self.value.is_empty() && !self.focused {
            (
                Span::styled(&self.placeholder, Style::default().fg(theme.fg_muted)),
                false,
            )
        } else {
            (
                Span::styled(&self.value, Style::default().fg(theme.fg)),
                self.focused,
            )
        };

        let line = Line::from(vec![label_span, text_span]);
        let style = if self.focused {
            Style::default().bg(theme.bg_highlight)
        } else {
            Style::default().bg(theme.bg)
        };
        let p = Paragraph::new(line).style(style);
        frame.render_widget(p, area);

        // Show cursor
        if cursor_visible {
            let cursor_x = area.x + label.len() as u16 + self.display_cursor_pos() as u16;
            let cursor_y = area.y;
            if cursor_x < area.x + area.width {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }

    fn display_cursor_pos(&self) -> usize {
        // Count display width up to cursor position
        use unicode_width::UnicodeWidthStr;
        self.value[..self.cursor].width()
    }
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new("")
    }
}
