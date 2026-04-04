use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::tui::themes::Theme;

const VERSION: &str = concat!("kyoku v", env!("CARGO_PKG_VERSION"));

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, hints: &[(&str, &str)]) {
    let mut spans = Vec::new();

    for (i, (key, action)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme.fg_muted)));
        }
        spans.push(Span::styled(
            *key,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", action),
            Style::default().fg(theme.fg_dim),
        ));
    }

    let version_width = VERSION.width() as u16 + 2;
    let split = if area.width > version_width + 4 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(version_width)])
            .split(area)
    } else {
        // Not enough room — skip version
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(0)])
            .split(area)
    };

    let hints_line = Line::from(spans);
    let hints_p = Paragraph::new(hints_line).style(Style::default().bg(theme.bg_alt));
    frame.render_widget(hints_p, split[0]);

    if split[1].width > 0 {
        let version_line = Line::from(vec![
            Span::styled(
                format!(" {} ", VERSION),
                Style::default().fg(theme.fg_muted),
            ),
        ]);
        let version_p = Paragraph::new(version_line)
            .style(Style::default().bg(theme.bg_alt))
            .alignment(ratatui::layout::Alignment::Right);
        frame.render_widget(version_p, split[1]);
    }
}
