use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::themes::Theme;

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

    let line = Line::from(spans);
    let p = Paragraph::new(line).style(Style::default().bg(theme.bg_alt));
    frame.render_widget(p, area);
}
