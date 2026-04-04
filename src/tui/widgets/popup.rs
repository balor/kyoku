use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::themes::Theme;

/// Render a centered popup overlay with a title and content.
pub fn render_popup(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: &str,
    content: &[Line<'_>],
    width_pct: u16,
    height: u16,
) -> Rect {
    let popup_area = centered_rect(width_pct, height, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_bright))
        .style(Style::default().bg(theme.bg_alt));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let p = Paragraph::new(content.to_vec()).style(Style::default().fg(theme.fg));
    frame.render_widget(p, inner);

    inner
}

fn centered_rect(width_pct: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);

    let width = (area.width as u32 * width_pct as u32 / 100) as u16;
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1]);

    horizontal[1]
}
