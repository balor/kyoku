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
            let max = self.filtered_indices().len();
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
        let filtered = self.filtered_indices();

        let (coll_id, coll_name) = if !filtered.is_empty() {
            let c = &self.suggestions[filtered[self.suggestion_selected]];
            (c.id, c.name.clone())
        } else {
            let name = self.input.value.trim().to_string();
            if name.is_empty() {
                return String::new();
            }
            match queries::find_collection_by_name(conn, &name) {
                Ok(Some(id)) => (id, name),
                Ok(None) => match queries::create_collection(conn, &name) {
                    Ok(id) => (id, name),
                    Err(_) => return "Failed to create collection".to_string(),
                },
                // A failed lookup is NOT "doesn't exist" — creating here
                // would duplicate the collection on a transient DB error.
                Err(_) => return "Failed to look up collection".to_string(),
            }
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

        let filtered = self.filtered_indices();
        let hint = if filtered.is_empty() {
            if self.input.value.trim().is_empty() {
                " No collections yet — type a name to create one".to_string()
            } else {
                " Enter to create new collection".to_string()
            }
        } else {
            format!(
                " {} match(es) — j/k to select, Enter to add",
                filtered.len()
            )
        };
        let p = Paragraph::new(Span::styled(hint, Style::default().fg(theme.fg_muted)));
        frame.render_widget(p, chunks[1]);

        let mut lines: Vec<Line<'_>> = Vec::new();
        for (pos, &i) in filtered.iter().enumerate() {
            let coll = &self.suggestions[i];
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
            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(coll.name.clone(), style),
                Span::styled(
                    format!(" ({} tracks)", coll.track_count),
                    Style::default().fg(theme.fg_dim),
                ),
            ]));
        }
        let p = Paragraph::new(lines);
        frame.render_widget(p, chunks[2]);
    }
}
