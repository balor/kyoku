use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::app::AppAction;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::popup;

#[derive(Default)]
pub struct HelpOverlay {
    pub visible: bool,
    pub scroll: usize,
    /// Last viewport-aware `max_scroll` reported by the renderer. Key handling
    /// clamps against this so pressing `j` at the bottom doesn't inflate an
    /// invisible counter that has to "wind back" before up-scrolling works.
    max_scroll: usize,
}

impl HelpOverlay {
    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) || keys::is_help(&key) || keys::is_quit(&key) {
            self.visible = false;
            self.scroll = 0;
            return AppAction::None;
        }
        let max = self.max_scroll;
        if keys::is_down(&key) {
            self.scroll = (self.scroll + 1).min(max);
        } else if keys::is_up(&key) {
            self.scroll = self.scroll.saturating_sub(1);
        } else if matches!(key.code, KeyCode::PageDown)
            || (key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL))
            || (key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.scroll = (self.scroll + 10).min(max);
        } else if matches!(key.code, KeyCode::PageUp)
            || (key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL))
            || (key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.scroll = self.scroll.saturating_sub(10);
        } else if matches!(key.code, KeyCode::Home | KeyCode::Char('g')) {
            self.scroll = 0;
        } else if matches!(key.code, KeyCode::End | KeyCode::Char('G')) {
            self.scroll = max;
        }
        AppAction::None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let bindings = help_content(theme);

        // Popup geometry
        let popup_height = area.height.saturating_sub(4).min(40);
        let footer_height: u16 = 2; // separator + hint
        let content_height = (popup_height as usize)
            .saturating_sub(2) // borders
            .saturating_sub(footer_height as usize);

        let max_scroll = bindings.len().saturating_sub(content_height);
        // Persist + clamp — so out-of-bounds values from a prior layout never
        // stick, and the key-handler has a fresh bound to clamp against.
        self.max_scroll = max_scroll;
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let scroll = self.scroll;

        let title = format!("Key Bindings  [{}/{}]", scroll + 1, max_scroll + 1);

        let inner = popup::render_popup(
            frame,
            area,
            theme,
            &title,
            &[],
            70,
            popup_height,
        );

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(footer_height),
            ])
            .split(inner);

        let visible: Vec<Line<'_>> = bindings
            .into_iter()
            .skip(scroll)
            .take(chunks[0].height as usize)
            .collect();

        let content_p = Paragraph::new(visible).style(Style::default().fg(theme.fg));
        frame.render_widget(content_p, chunks[0]);

        // Pinned footer
        let separator = Line::from(Span::styled(
            "─".repeat(chunks[1].width as usize),
            Style::default().fg(theme.border),
        ));
        let hint = Line::from(vec![
            Span::styled(" j/k", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" scroll · ", Style::default().fg(theme.fg_dim)),
            Span::styled("Ctrl+D/U", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" page · ", Style::default().fg(theme.fg_dim)),
            Span::styled("g/G", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" top/bottom · ", Style::default().fg(theme.fg_dim)),
            Span::styled("Esc/?", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" close", Style::default().fg(theme.fg_dim)),
        ]);
        let footer_p = Paragraph::new(vec![separator, hint]).style(Style::default().fg(theme.fg));
        frame.render_widget(footer_p, chunks[1]);
    }
}

fn help_content(theme: &Theme) -> Vec<Line<'static>> {
    let accent = theme.accent;
    let dim = theme.fg_dim;

    let section = |title: &'static str| -> Line<'static> {
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ))
    };

    let binding = |key: &'static str, desc: &'static str| -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("  {:14}", key),
                Style::default()
                    .fg(accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(desc, Style::default().fg(dim)),
        ])
    };

    vec![
        section("Global (any view)"),
        binding("q / Ctrl+C", "Quit"),
        binding("? / F1", "Toggle this help overlay"),
        binding("F5 / Ctrl+R", "Refresh (rescan inbox, reload view)"),
        binding("/", "Focus local filter (filters current list)"),
        binding("g", "Open global search (albums, tracks, collections)"),
        binding("Esc", "Back / cancel / clear focus"),
        binding("Tab", "Switch Albums ↔ Collections"),
        Line::from(""),
        section("Navigation (lists)"),
        binding("j / ↓", "Move down"),
        binding("k / ↑", "Move up"),
        binding("Ctrl+D", "Half page down"),
        binding("Ctrl+U", "Half page up"),
        binding("Ctrl+F / PgDn", "Page down"),
        binding("Ctrl+B / PgUp", "Page up"),
        binding("G", "Jump to bottom"),
        binding("Enter", "Open / confirm selection"),
        Line::from(""),
        section("Multi-select (all list views)"),
        binding("Space", "Toggle row, advance cursor"),
        binding("Esc", "Clear selection (when non-empty)"),
        binding("d", "Delete cursor row, or selection if non-empty"),
        Line::from(""),
        section("Library Browser"),
        binding("Enter", "Open album detail"),
        binding("i", "Start import wizard"),
        binding("O", "Organize entire library (preview + apply)"),
        binding("p", "Play album (or marked albums) in external player"),
        binding("a", "Add whole album to a collection"),
        binding("s", "Sort (cycle: artist, album, year, tracks)"),
        binding("S", "Toggle sort direction (asc/desc)"),
        binding("d", "Delete album(s) (with file-removal opt-in)"),
        Line::from(""),
        section("Album Detail (tracks)"),
        binding("e", "Edit selected track tags"),
        binding("R", "Rename album"),
        binding("O", "Organize this album (preview + apply)"),
        binding("C", "Fetch cover art from MusicBrainz"),
        binding("p", "Play track (or marked tracks) in external player"),
        binding("P", "Play whole album in external player"),
        binding("a", "Add selected track to a collection"),
        binding("o", "Open file location in system file manager"),
        binding("d", "Delete track(s) (with file-removal opt-in)"),
        Line::from(""),
        section("Collections"),
        binding("n", "Create new collection"),
        binding("R", "Rename collection"),
        binding("O", "Organize all collections (preview + apply)"),
        binding("p", "Play collection (or marked) in external player"),
        binding("d", "Delete collection(s) (with file-removal opt-in)"),
        binding("Enter", "Browse collection"),
        Line::from(""),
        section("Collection Detail (tracks)"),
        binding("e", "Edit selected track tags"),
        binding("R", "Rename collection"),
        binding("O", "Organize this collection (preview + apply)"),
        binding("p", "Play track (or marked tracks) in external player"),
        binding("P", "Play whole collection in external player"),
        binding("o", "Open file location in system file manager"),
        binding("x", "Remove track from collection (with file-removal opt-in)"),
        binding("d", "Delete track(s) from library (with file-removal opt-in)"),
    ]
}
