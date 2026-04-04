use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::app::AppAction;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::popup;

#[derive(Default)]
pub struct HelpOverlay {
    pub visible: bool,
    pub scroll: usize,
}

impl HelpOverlay {
    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) || keys::is_help(&key) || keys::is_quit(&key) {
            self.visible = false;
            return AppAction::None;
        }
        if keys::is_down(&key) {
            self.scroll = self.scroll.saturating_add(1);
        }
        if keys::is_up(&key) {
            self.scroll = self.scroll.saturating_sub(1);
        }
        AppAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let bindings = help_content(theme);

        let visible: Vec<Line<'_>> = bindings
            .into_iter()
            .skip(self.scroll)
            .collect();

        popup::render_popup(frame, area, theme, "Key Bindings", &visible, 70, area.height.saturating_sub(4).min(30));
    }
}

fn help_content(theme: &Theme) -> Vec<Line<'static>> {
    let accent = theme.accent;
    let dim = theme.fg_dim;
    let _muted = theme.fg_muted;

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
        section("Global"),
        binding("q / Ctrl+C", "Quit"),
        binding("? / F1", "Help overlay"),
        binding("/", "Focus local filter (filters current list)"),
        binding("g", "Open global search (albums, tracks, collections)"),
        binding("Esc", "Back / cancel / clear search"),
        binding("Tab", "Switch Albums ↔ Collections"),
        Line::from(""),
        section("Library Browser"),
        binding("j / ↓", "Move down"),
        binding("k / ↑", "Move up"),
        binding("Ctrl+D", "Half page down"),
        binding("Ctrl+U", "Half page up"),
        binding("Ctrl+F / PgDn", "Page down"),
        binding("Ctrl+B / PgUp", "Page up"),
        binding("G", "Jump to bottom"),
        binding("Enter", "Open album detail"),
        binding("i", "Start import wizard"),
        binding("a", "Add whole album to a collection"),
        binding("s", "Sort (cycle: artist, album, year, tracks)"),
        binding("c", "Switch to collections"),
        Line::from(""),
        section("Album Detail"),
        binding("e", "Edit track tags"),
        binding("R", "Rename album"),
        binding("a", "Add selected track to a collection"),
        binding("o", "Open file location"),
        binding("Esc", "Back to library"),
        Line::from(""),
        section("Collections"),
        binding("n", "Create new collection"),
        binding("R", "Rename collection"),
        binding("d", "Delete collection (confirms)"),
        binding("Enter", "Browse collection"),
        binding("Tab", "Switch to albums"),
        Line::from(""),
        section("Collection Detail"),
        binding("e", "Edit track tags"),
        binding("R", "Rename collection"),
        binding("x", "Remove track from collection (confirms)"),
        binding("Esc", "Back to collections"),
        Line::from(""),
        section("Import Wizard"),
        binding("A", "Accept as-is"),
        binding("S", "Skip"),
        binding("L", "Import loose"),
        binding("n / p", "Next / previous group"),
        binding("Enter", "Confirm and import"),
        binding("Esc", "Cancel"),
        Line::from(""),
        section("Tag Editor"),
        binding("Enter", "Edit selected field"),
        binding("Tab", "Next field"),
        binding("Ctrl+S", "Save changes"),
        binding("Esc", "Cancel"),
    ]
}
