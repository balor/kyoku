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

pub struct AddToCollectionPopup {
    pub input: TextInput,
    pub track_ids: Vec<i64>,
    pub display_name: String,
    pub suggestions: Vec<CollectionRow>,
    pub suggestion_selected: usize,
}

pub enum PopupAction {
    /// Popup is still open, no action needed.
    None,
    /// Popup has closed, optionally with a status notice to show the user.
    Closed(Option<String>),
}

impl AddToCollectionPopup {
    pub fn open(track_ids: Vec<i64>, display_name: String, conn: &Connection) -> Self {
        let mut input = TextInput::new("Collection name (new or existing)...").with_label(" + ");
        input.focused = true;
        Self {
            input,
            track_ids,
            display_name,
            suggestions: queries::list_collections(conn).unwrap_or_default(),
            suggestion_selected: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) -> PopupAction {
        if keys::is_back(&key) {
            return PopupAction::Closed(None);
        }

        // Arrow-only navigation: the name input is live, so `j`/`k` must
        // insert ("Jazz", "J-Pop"), not move the suggestion cursor.
        if keys::is_up_arrow(&key) {
            if self.suggestion_selected > 0 {
                self.suggestion_selected -= 1;
            }
            return PopupAction::None;
        }
        if keys::is_down_arrow(&key) {
            let max = self.visible_entries().len();
            if max > 0 && self.suggestion_selected < max.saturating_sub(1) {
                self.suggestion_selected += 1;
            }
            return PopupAction::None;
        }

        if keys::is_confirm(&key) {
            return PopupAction::Closed(Some(self.commit(conn)));
        }

        if self.input.handle_key(key) {
            self.suggestion_selected = 0;
        }
        PopupAction::None
    }

    fn commit(&mut self, conn: &Connection) -> String {
        let entries = self.visible_entries();

        let (coll_id, coll_name) = match entries.get(self.suggestion_selected) {
            Some(AddEntry::Existing(idx)) => {
                let c = &self.suggestions[*idx];
                (c.id, c.name.clone())
            }
            Some(AddEntry::NewTyped(name)) => match queries::get_or_create_collection(conn, name) {
                Ok((id, _)) => (id, name.clone()),
                Err(_) => return "Failed to create collection".to_string(),
            },
            None => return String::new(),
        };

        let added =
            queries::add_tracks_to_collection_ordered(conn, coll_id, &self.track_ids).unwrap_or(0);
        let skipped = self.track_ids.len().saturating_sub(added as usize) as u32;

        if self.track_ids.len() == 1 {
            if added > 0 {
                format!("Added to '{}'", coll_name)
            } else {
                format!("Already in '{}'", coll_name)
            }
        } else if skipped == 0 {
            format!("Added {} track(s) to '{}'", added, coll_name)
        } else if added == 0 {
            format!("All {} track(s) already in '{}'", skipped, coll_name)
        } else {
            format!("Added {} · {} already in '{}'", added, skipped, coll_name)
        }
    }

    fn visible_entries(&self) -> Vec<AddEntry> {
        let typed = self.input.value.trim();
        let mut entries = Vec::new();
        if !typed.is_empty()
            && !self
                .suggestions
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(typed))
        {
            entries.push(AddEntry::NewTyped(typed.to_string()));
        }
        entries.extend(self.filtered_indices().into_iter().map(AddEntry::Existing));
        entries
    }

    fn filtered_indices(&self) -> Vec<usize> {
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

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let title = format!("Add '{}' to collection", self.display_name);
        let content = vec![Line::from("")];
        let inner = popup::render_popup(frame, area, theme, &title, &content, 60, 15);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(3),
            ])
            .split(inner);

        self.input.render(frame, chunks[0], theme);

        let entries = self.visible_entries();
        let hint = if entries.is_empty() {
            " Type a name and press Enter to create".to_string()
        } else {
            format!(" {} option(s) — ↑↓ navigate, Enter to add", entries.len())
        };
        let p = Paragraph::new(Span::styled(hint, Style::default().fg(theme.fg_muted)));
        frame.render_widget(p, chunks[1]);

        let mut lines: Vec<Line<'_>> = Vec::new();
        for (pos, entry) in entries.iter().enumerate() {
            let selected = pos == self.suggestion_selected;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            match entry {
                AddEntry::NewTyped(name) => lines.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(
                        "[+ New] ",
                        Style::default()
                            .fg(theme.green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(name.clone(), style),
                    Span::styled(" (will be created)", Style::default().fg(theme.fg_muted)),
                ])),
                AddEntry::Existing(i) => {
                    let coll = &self.suggestions[*i];
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
enum AddEntry {
    NewTyped(String),
    Existing(usize),
}
