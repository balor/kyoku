use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::themes::Theme;
use crate::tui::widgets::popup;

/// A reusable two-stage delete confirmation popup with an opt-in
/// "also delete files from disk" checkbox.
#[derive(Debug, Clone)]
pub struct ConfirmDelete {
    pub title: String,
    pub primary: String,
    /// Optional summary line (e.g. "47 file(s) under /path").
    pub summary: Option<String>,
    /// Optional detail lines (muted, neutral colour). Rendered under the
    /// primary line — useful for per-album breakdowns in batch deletes.
    pub details: Vec<String>,
    /// Optional warning lines (yellow), e.g. "3 tracks will be removed entirely".
    pub warnings: Vec<String>,
    /// Whether the "also delete files" checkbox is currently checked.
    pub delete_files: bool,
    /// If false, the checkbox is hidden (no files to delete).
    pub show_checkbox: bool,
    /// Override text for the checkbox row (defaults to
    /// "Also delete files from disk").
    pub checkbox_label: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    None,
    Cancel,
    Confirm { delete_files: bool },
}

impl ConfirmDelete {
    pub fn new(title: impl Into<String>, primary: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            primary: primary.into(),
            summary: None,
            details: Vec::new(),
            warnings: Vec::new(),
            delete_files: false,
            show_checkbox: true,
            checkbox_label: None,
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn with_detail(mut self, line: impl Into<String>) -> Self {
        self.details.push(line.into());
        self
    }

    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    pub fn without_checkbox(mut self) -> Self {
        self.show_checkbox = false;
        self
    }

    pub fn with_checkbox_label(mut self, label: impl Into<String>) -> Self {
        self.checkbox_label = Some(label.into());
        self
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ConfirmAction {
        match key.code {
            KeyCode::Char(' ') | KeyCode::Char('t') if self.show_checkbox => {
                self.delete_files = !self.delete_files;
                ConfirmAction::None
            }
            KeyCode::Char('y') | KeyCode::Enter => ConfirmAction::Confirm {
                delete_files: self.show_checkbox && self.delete_files,
            },
            KeyCode::Esc => ConfirmAction::Cancel,
            _ => ConfirmAction::Cancel,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut content = vec![
            Line::from(""),
            Line::from(Span::styled(
                self.primary.clone(),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            )),
        ];

        if let Some(summary) = &self.summary {
            content.push(Line::from(""));
            content.push(Line::from(Span::styled(
                summary.clone(),
                Style::default().fg(theme.fg_dim),
            )));
        }

        if !self.details.is_empty() {
            content.push(Line::from(""));
            for line in &self.details {
                content.push(Line::from(Span::styled(
                    format!("  • {}", line),
                    Style::default().fg(theme.fg_dim),
                )));
            }
        }

        for warning in &self.warnings {
            content.push(Line::from(Span::styled(
                format!("⚠ {}", warning),
                Style::default().fg(theme.yellow),
            )));
        }

        if self.show_checkbox {
            content.push(Line::from(""));
            let mark = if self.delete_files { "[x]" } else { "[ ]" };
            let style = if self.delete_files {
                Style::default().fg(theme.red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let label = self
                .checkbox_label
                .as_deref()
                .unwrap_or("Also delete files from disk");
            content.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{} {}", mark, label), style),
            ]));
        }

        content.push(Line::from(""));
        let hint = if self.show_checkbox {
            "space=toggle, y/Enter=confirm, Esc=cancel"
        } else {
            "y/Enter=confirm, Esc=cancel"
        };
        content.push(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.fg_muted),
        )));

        // Calculate height: count of lines + 2 for borders
        let height = (content.len() as u16 + 2).min(area.height.saturating_sub(2));
        popup::render_popup(frame, area, theme, &self.title, &content, 70, height);
    }
}
