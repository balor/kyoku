//! Shared renderer for the organize-plan preview popup.
//!
//! Used by the library view, the collections list view, the collection detail
//! view, and the album detail view so every organize modal shares the same
//! look and scrolling behaviour (scrollable content + pinned footer with
//! stats and keybind hint).
//!
//! Two modes:
//! - [`OrganizeView::Summary`] — grouped by `(from_dir → to_dir)` / collection.
//! - [`OrganizeView::Details`] — per-file listing with filenames + orphans.
//!
//! Content never truncates; long plans scroll via the caller's `scroll` offset.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::core::organizer::OrganizePlan;
use crate::tui::themes::Theme;
use crate::tui::widgets::popup;

/// Which representation of the plan to render.
#[derive(Clone, Copy)]
pub enum OrganizeView {
    /// Compact grouped overview.
    Summary,
    /// Per-file listing with full paths and orphans.
    Details,
}

/// Render the organize-plan preview in the requested mode.
///
/// The caller provides its own `scroll` offset (in lines), the title shown
/// in the popup border (an arrow marker — ↑, ↓, or ↕ — is appended when the
/// content overflows to hint at off-screen lines), and the hint text for the
/// pinned footer.
/// Render the popup and return the viewport-aware `max_scroll` so the caller
/// can clamp its own scroll state inside `handle_key` (preventing runaway
/// counters when the user keeps pressing `j` at the bottom of the list).
pub fn render(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    plan: &OrganizePlan,
    scroll: &mut usize,
    view: OrganizeView,
    title: &str,
    hint: &str,
    width_pct: u16,
) -> usize {
    let all_lines = match view {
        OrganizeView::Summary => build_summary_lines(plan, theme),
        OrganizeView::Details => build_detail_lines(plan, theme),
    };

    // Pinned footer: stats line + hint line, always visible. Zero counts are
    // hidden so the line reads cleanly ("267 moves" instead of
    // "267 move(s) · 0 copy/copies · 0 in place").
    let mut stats: Vec<String> = Vec::new();
    if !plan.moves.is_empty() {
        stats.push(pluralize(plan.moves.len(), "move", "moves"));
    }
    if !plan.copies.is_empty() {
        stats.push(pluralize(plan.copies.len(), "copy", "copies"));
    }
    if plan.skipped > 0 {
        stats.push(format!("{} already in place", plan.skipped));
    }
    if !plan.missing_sources.is_empty() {
        stats.push(pluralize(
            plan.missing_sources.len(),
            "orphan",
            "orphans",
        ));
    }
    if stats.is_empty() {
        stats.push("nothing to do".to_string());
    }
    let stats_line = Line::from(Span::styled(
        format!(" {}", stats.join(" · ")),
        Style::default().fg(theme.fg_muted),
    ));
    let hint_line = Line::from(Span::styled(
        hint.to_string(),
        Style::default().fg(theme.fg_muted),
    ));
    let footer_height: u16 = 3; // separator + stats + hint

    // Popup sizing: fit content when short, else fill the area minus a margin.
    let desired_height = (all_lines.len() as u16)
        .saturating_add(2) // popup borders
        .saturating_add(footer_height);
    let popup_height = desired_height.min(area.height.saturating_sub(4));
    let content_height = (popup_height as usize)
        .saturating_sub(2)
        .saturating_sub(footer_height as usize);
    let max_scroll = all_lines.len().saturating_sub(content_height);
    // Clamp caller's scroll offset in place so unbounded `down` keypresses don't
    // leave the user pressing `up` many times before the view responds.
    if *scroll > max_scroll {
        *scroll = max_scroll;
    }
    let scroll = *scroll;

    let rendered_title = if max_scroll > 0 {
        let marker = match (scroll > 0, scroll < max_scroll) {
            (true, true) => " ↕",
            (false, true) => " ↓",
            (true, false) => " ↑",
            _ => "",
        };
        format!("{}{}", title, marker)
    } else {
        title.to_string()
    };

    let inner = popup::render_popup(
        frame,
        area,
        theme,
        &rendered_title,
        &[],
        width_pct,
        popup_height,
    );

    // Split inner into scrollable content + pinned footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(footer_height)])
        .split(inner);

    let visible: Vec<Line<'_>> = all_lines
        .into_iter()
        .skip(scroll)
        .take(chunks[0].height as usize)
        .collect();
    let content_p = Paragraph::new(visible).style(Style::default().fg(theme.fg));
    frame.render_widget(content_p, chunks[0]);

    let separator = Line::from(Span::styled(
        "─".repeat(chunks[1].width as usize),
        Style::default().fg(theme.border),
    ));
    let footer_p = Paragraph::new(vec![separator, stats_line, hint_line])
        .style(Style::default().fg(theme.fg));
    frame.render_widget(footer_p, chunks[1]);

    max_scroll
}

fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{} {}", count, singular)
    } else {
        format!("{} {}", count, plural)
    }
}

fn build_summary_lines<'a>(plan: &'a OrganizePlan, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    lines.push(Line::from(""));

    let nothing_to_do = plan.moves.is_empty() && plan.copies.is_empty();

    if nothing_to_do {
        lines.push(Line::from(Span::styled(
            format!(
                "Nothing to organize — {} already in place.",
                pluralize(plan.skipped, "file", "files")
            ),
            Style::default().fg(theme.fg_muted),
        )));
        return lines;
    }

    if !plan.moves.is_empty() {
        // Split moves into cross-directory moves and in-place renames so the
        // latter don't render as confusing "from → to" where both paths are
        // identical (the move is just a filename change).
        let mut dir_groups: std::collections::BTreeMap<(String, String), usize> =
            std::collections::BTreeMap::new();
        let mut rename_groups: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for m in &plan.moves {
            let from_dir = m
                .from
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let to_dir = m
                .to
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if from_dir == to_dir {
                *rename_groups.entry(from_dir).or_insert(0) += 1;
            } else {
                *dir_groups.entry((from_dir, to_dir)).or_insert(0) += 1;
            }
        }
        lines.push(Line::from(Span::styled(
            format!(" {} to move:", pluralize(plan.moves.len(), "file", "files")),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD),
        )));
        // Count is shown BEFORE the path ("5×  /path") to avoid the trailing
        // "(N)" reading as part of the directory name.
        for ((from_dir, to_dir), count) in &dir_groups {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {:>3}× ", count),
                    Style::default().fg(theme.fg_muted),
                ),
                Span::styled(from_dir.clone(), Style::default().fg(theme.fg_dim)),
            ]));
            lines.push(Line::from(Span::styled(
                format!("        → {}", to_dir),
                Style::default().fg(theme.accent),
            )));
        }
        if !rename_groups.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " renamed in place:",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            )));
            for (dir, count) in &rename_groups {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("   {:>3}× ", count),
                        Style::default().fg(theme.fg_muted),
                    ),
                    Span::styled(dir.clone(), Style::default().fg(theme.fg_dim)),
                ]));
            }
        }
    }

    if !plan.copies.is_empty() {
        // Group collection copies by collection name with counts.
        let mut coll_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for c in &plan.copies {
            *coll_counts.entry(c.collection_name.clone()).or_insert(0) += 1;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                " {} to copy into collections:",
                pluralize(plan.copies.len(), "file", "files")
            ),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD),
        )));
        for (name, count) in &coll_counts {
            lines.push(Line::from(Span::styled(
                format!("   {} ({})", name, pluralize(*count, "file", "files")),
                Style::default().fg(theme.accent_alt),
            )));
        }
    }

    lines
}

fn build_detail_lines<'a>(plan: &'a OrganizePlan, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();

    let nothing_to_do =
        plan.moves.is_empty() && plan.copies.is_empty() && plan.missing_sources.is_empty();
    if nothing_to_do {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Nothing to organize — {} already in place.",
                pluralize(plan.skipped, "file", "files")
            ),
            Style::default().fg(theme.fg_muted),
        )));
        return lines;
    }

    if !plan.moves.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" Moves ({}):", plan.moves.len()),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        lines.push(Line::from(""));
        for m in &plan.moves {
            let from_name = m
                .from
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("?");
            let from_dir = m
                .from
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let to_dir = m
                .to
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let to_name = m.to.file_name().and_then(|f| f.to_str()).unwrap_or("?");

            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(from_name, Style::default().fg(theme.fg)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("    from: ", Style::default().fg(theme.fg_muted)),
                Span::styled(from_dir, Style::default().fg(theme.fg_dim)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("    → to: ", Style::default().fg(theme.fg_muted)),
                Span::styled(to_dir, Style::default().fg(theme.accent)),
                Span::styled(
                    if from_name != to_name {
                        format!("/{}", to_name)
                    } else {
                        String::new()
                    },
                    Style::default().fg(theme.yellow),
                ),
            ]));
            // Note: `m.also_collection` is only set when the move destination
            // is already inside that collection's folder, so surfacing it here
            // reads as redundant — it's used downstream by apply_organize to
            // update collection_tracks.collection_file_path.
            lines.push(Line::from(""));
        }
    }

    if !plan.copies.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" Collection copies ({}):", plan.copies.len()),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        lines.push(Line::from(""));
        for c in &plan.copies {
            let name = c.to.file_name().and_then(|f| f.to_str()).unwrap_or("?");
            let to_dir = c
                .to
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", name), Style::default().fg(theme.fg)),
                Span::styled(
                    format!("→ {}", to_dir),
                    Style::default().fg(theme.accent_alt),
                ),
                Span::styled(
                    format!(" ({})", c.collection_name),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    if !plan.missing_sources.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                " Orphaned tracks ({} — will be pruned):",
                plan.missing_sources.len()
            ),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for (id, path, title) in &plan.missing_sources {
            lines.push(Line::from(Span::styled(
                format!("  [{}] {} — {}", id, title, path.display()),
                Style::default().fg(theme.fg_dim),
            )));
        }
        lines.push(Line::from(""));
    }

    lines
}

