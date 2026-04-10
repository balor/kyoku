use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rusqlite::Connection;

use crate::db::queries::{self, CollectionRow};
use crate::tui::fuzzy;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::popup;

/// A reusable picker for selecting (or creating) a collection name.
///
/// Unlike `AddToCollectionPopup`, this widget does NOT touch the database.
/// It returns the chosen collection name as a string and lets the caller
/// decide what to do with it (e.g. set a per-group target without committing).
///
/// The picker shows:
/// - A "+ New: <default_name>" entry pinned at the top (the directory/folder name)
/// - All existing collections (filtered by what the user types)
/// - A text input for free-form names
pub struct PickCollectionPopup {
    pub input: TextInput,
    /// Title shown in the popup header (e.g. "Add group to collection").
    pub title: String,
    /// Default name proposal (the import group's directory name).
    pub default_name: String,
    /// All known collections, sorted by name.
    pub suggestions: Vec<CollectionRow>,
    /// Highlighted entry in the visible list (0 = the default-name proposal).
    pub selected: usize,
}

pub enum PickAction {
    /// Popup is still open.
    None,
    /// Cancelled — caller should leave the field unchanged.
    Cancel,
    /// User picked or typed a name. Empty string means "clear assignment".
    Picked(String),
}

impl PickCollectionPopup {
    pub fn open(
        title: impl Into<String>,
        default_name: impl Into<String>,
        current_value: &str,
        conn: &Connection,
    ) -> Self {
        let mut input =
            TextInput::new("Type a name or pick from the list").with_label(" Coll: ");
        input.value = current_value.to_string();
        input.cursor = input.value.len();
        input.focused = true;

        Self {
            input,
            title: title.into(),
            default_name: default_name.into(),
            suggestions: queries::list_collections(conn).unwrap_or_default(),
            selected: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickAction {
        if keys::is_back(&key) {
            return PickAction::Cancel;
        }

        if keys::is_up(&key) {
            if self.selected > 0 {
                self.selected -= 1;
            }
            return PickAction::None;
        }
        if keys::is_down(&key) {
            let max = self.visible_count();
            if max > 0 && self.selected + 1 < max {
                self.selected += 1;
            }
            return PickAction::None;
        }

        if keys::is_confirm(&key) {
            return PickAction::Picked(self.commit());
        }

        if self.input.handle_key(key) {
            self.selected = 0;
        }
        PickAction::None
    }

    /// Returns the name picked or typed by the user.
    /// Empty string = "clear assignment".
    fn commit(&self) -> String {
        let entries = self.visible_entries();
        match entries.get(self.selected) {
            Some(Entry::NewTyped(s)) => s.clone(),
            Some(Entry::NewDefault) => self.default_name.clone(),
            Some(Entry::Existing(idx)) => self.suggestions[*idx].name.clone(),
            None => String::new(),
        }
    }

    fn filtered_suggestion_indices(&self) -> Vec<usize> {
        let q = self.input.value.trim();
        if q.is_empty() {
            return (0..self.suggestions.len()).collect();
        }
        self.suggestions
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                fuzzy::matches_any(q, &[&c.name, c.description.as_deref().unwrap_or("")])
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn visible_entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        let typed = self.input.value.trim();

        // Pinned "create new" entry at the top:
        // - If user has typed text that doesn't match an existing collection → use typed text
        // - Else if the folder default isn't already a collection → use default
        if !typed.is_empty() {
            let exact_match = self
                .suggestions
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(typed));
            if !exact_match {
                entries.push(Entry::NewTyped(typed.to_string()));
            }
        } else {
            let default_already_exists = self
                .suggestions
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(&self.default_name));
            if !default_already_exists && !self.default_name.trim().is_empty() {
                entries.push(Entry::NewDefault);
            }
        }

        for idx in self.filtered_suggestion_indices() {
            entries.push(Entry::Existing(idx));
        }
        entries
    }

    fn visible_count(&self) -> usize {
        self.visible_entries().len()
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let content = vec![Line::from("")];
        let inner = popup::render_popup(frame, area, theme, &self.title, &content, 70, 18);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // input
                Constraint::Length(1), // hint
                Constraint::Min(3),   // suggestions
            ])
            .split(inner);

        self.input.render(frame, chunks[0], theme);

        let entries = self.visible_entries();
        let hint = if entries.is_empty() {
            " Type a name and press Enter to assign".to_string()
        } else {
            format!(
                " {} option(s) — ↑↓ navigate, Enter pick, Esc cancel",
                entries.len()
            )
        };
        let p = Paragraph::new(Span::styled(hint, Style::default().fg(theme.fg_muted)));
        frame.render_widget(p, chunks[1]);

        // Render the entries
        let mut lines: Vec<Line<'_>> = Vec::new();
        for (pos, entry) in entries.iter().enumerate() {
            let is_selected = pos == self.selected;
            let marker = if is_selected { "▶ " } else { "  " };
            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };

            match entry {
                Entry::NewTyped(s) => {
                    lines.push(Line::from(vec![
                        Span::styled(marker, style),
                        Span::styled(
                            "[+ New] ",
                            Style::default()
                                .fg(theme.green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(s.clone(), style),
                        Span::styled(
                            " (will be created)",
                            Style::default().fg(theme.fg_muted),
                        ),
                    ]));
                }
                Entry::NewDefault => {
                    lines.push(Line::from(vec![
                        Span::styled(marker, style),
                        Span::styled(
                            "[+ New] ",
                            Style::default()
                                .fg(theme.green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(self.default_name.clone(), style),
                        Span::styled(
                            " (from folder name)",
                            Style::default().fg(theme.fg_muted),
                        ),
                    ]));
                }
                Entry::Existing(idx) => {
                    let coll = &self.suggestions[*idx];
                    lines.push(Line::from(vec![
                        Span::styled(marker, style),
                        Span::styled(coll.name.clone(), style),
                        Span::styled(
                            format!(" ({} tracks)", coll.track_count),
                            Style::default().fg(theme.fg_dim),
                        ),
                    ]));
                }
            }
        }
        let p = Paragraph::new(lines);
        frame.render_widget(p, chunks[2]);
    }
}

#[derive(Debug, Clone)]
enum Entry {
    /// "Create new" with the user's typed text
    NewTyped(String),
    /// "Create new" with the group's folder name (when no text typed)
    NewDefault,
    /// Pick an existing collection by index into `suggestions`
    Existing(usize),
}
