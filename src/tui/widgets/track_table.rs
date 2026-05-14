use std::fmt::Display;

use ratatui::layout::Constraint;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Cell;

use crate::tui::themes::Theme;

const GUTTER_WIDTH: u16 = 1;
const NUMBER_WIDTH: u16 = 4;
const YEAR_WIDTH: u16 = 6;
const COUNT_WIDTH: u16 = 8;
const DURATION_WIDTH: u16 = 10;

pub fn header_cell(label: impl Into<String>, theme: &Theme) -> Cell<'static> {
    Cell::from(Span::styled(
        label.into(),
        Style::default().fg(theme.accent),
    ))
}

pub fn right_header_cell(label: impl Display, width: u16, theme: &Theme) -> Cell<'static> {
    Cell::from(Span::styled(
        format!("{:>width$}", label, width = width as usize),
        Style::default().fg(theme.accent),
    ))
}

pub fn numeric_header_cell(label: impl Display, theme: &Theme) -> Cell<'static> {
    right_header_cell(label, NUMBER_WIDTH, theme)
}

pub fn right_cell(value: impl Display, width: u16) -> Cell<'static> {
    Cell::from(format!("{:>width$}", value, width = width as usize))
}

pub fn numeric_cell(value: impl Display) -> Cell<'static> {
    right_cell(value, NUMBER_WIDTH)
}

pub fn count_header_cell(label: impl Display, theme: &Theme) -> Cell<'static> {
    right_header_cell(label, COUNT_WIDTH, theme)
}

pub fn count_cell(value: impl Display) -> Cell<'static> {
    right_cell(value, COUNT_WIDTH)
}

pub fn year_cell(value: impl Display) -> Cell<'static> {
    right_cell(value, YEAR_WIDTH)
}

pub fn duration_header_cell(label: impl Display, theme: &Theme) -> Cell<'static> {
    right_header_cell(label, DURATION_WIDTH, theme)
}

pub fn duration_cell(value: impl Display) -> Cell<'static> {
    right_cell(value, DURATION_WIDTH)
}

pub fn blank_numeric_cell() -> Cell<'static> {
    Cell::from(" ".repeat(NUMBER_WIDTH as usize))
}

pub fn row_style(theme: &Theme, visual_index: usize, selected: bool) -> Style {
    if selected {
        Style::default()
            .bg(theme.bg_selected)
            .fg(theme.fg)
            .add_modifier(Modifier::BOLD)
    } else if visual_index.is_multiple_of(2) {
        Style::default().bg(theme.bg).fg(theme.fg)
    } else {
        Style::default().bg(theme.bg_alt).fg(theme.fg)
    }
}

pub fn album_list_widths() -> [Constraint; 6] {
    [
        Constraint::Length(GUTTER_WIDTH),
        Constraint::Fill(2),
        Constraint::Fill(3),
        Constraint::Length(YEAR_WIDTH),
        Constraint::Length(COUNT_WIDTH),
        Constraint::Length(8),
    ]
}

pub fn collection_list_widths() -> [Constraint; 4] {
    [
        Constraint::Length(GUTTER_WIDTH),
        Constraint::Fill(1),
        Constraint::Length(COUNT_WIDTH),
        Constraint::Length(DURATION_WIDTH),
    ]
}

pub fn album_detail_widths() -> [Constraint; 6] {
    [
        Constraint::Length(GUTTER_WIDTH),
        Constraint::Length(NUMBER_WIDTH),
        Constraint::Fill(1),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(10),
    ]
}

pub fn collection_detail_widths() -> [Constraint; 6] {
    [
        Constraint::Length(GUTTER_WIDTH),
        Constraint::Length(NUMBER_WIDTH),
        Constraint::Fill(3),
        Constraint::Fill(2),
        Constraint::Length(8),
        Constraint::Length(6),
    ]
}
