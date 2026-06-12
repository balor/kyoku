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

use crate::core::organize_preview::{
    self, DetailPreview, PlanStats, SummaryPreview,
};
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
    let blocked = plan.prune_blocked_reason.as_deref();
    let all_lines = match view {
        OrganizeView::Summary => {
            build_summary_lines(&organize_preview::build_summary(plan), blocked, theme)
        }
        OrganizeView::Details => {
            build_detail_lines(&organize_preview::build_details(plan), blocked, theme)
        }
    };

    // Pinned footer: stats line + hint line, always visible. Zero counts are
    // hidden so the line reads cleanly ("267 moves" instead of
    // "267 move(s) · 0 copy/copies · 0 in place").
    // Pull the summary's stats (it's a small allocation) so the footer
    // numbers match the body view — same split of dangling vs absorbed
    // orphans, instead of a raw total that'd contradict the listing.
    let stats_text = format_stats_line(organize_preview::build_summary(plan).stats);
    let stats_line = Line::from(Span::styled(
        format!(" {}", stats_text),
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

/// Plain-text stats line shared with the CLI renderer. Zero counts are
/// omitted so the line reads cleanly ("267 moves" instead of
/// "267 moves · 0 copies · 0 in place").
pub fn format_stats_line(stats: PlanStats) -> String {
    let mut parts: Vec<String> = Vec::new();
    if stats.moves_total > 0 {
        parts.push(pluralize(stats.moves_total, "move", "moves"));
    }
    if stats.copies_total > 0 {
        parts.push(pluralize(stats.copies_total, "copy", "copies"));
    }
    if stats.skipped > 0 {
        parts.push(format!("{} already in place", stats.skipped));
    }
    if stats.orphans > 0 {
        parts.push(pluralize(stats.orphans, "orphan", "orphans"));
    }
    if stats.file_orphans > 0 {
        parts.push(pluralize(
            stats.file_orphans,
            "dangling orphan",
            "dangling orphans",
        ));
    }
    if stats.file_orphans_absorbed > 0 {
        // Absorbed orphans are the ones about to be overwritten by a
        // move — surface them so the user sees "these moves replace
        // existing files" without having to correlate two sections.
        parts.push(format!(
            "{} overwrite(s) during move",
            stats.file_orphans_absorbed
        ));
    }
    if parts.is_empty() {
        return "nothing to do".to_string();
    }
    parts.join(" · ")
}

/// Warning block shown at the top of both views when the missing-source
/// prune is blocked (probably-unavailable volume — see `plan_organize`).
/// Placed first so it survives the "nothing to do" early return: an
/// unmounted volume often means the plan contains ONLY missing sources.
fn blocked_warning_lines<'a>(reason: &str, theme: &Theme) -> Vec<Line<'a>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            " ⚠ missing-source prune blocked".to_string(),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("   {}", reason),
            Style::default().fg(theme.fg_muted),
        )),
    ]
}

fn build_summary_lines<'a>(
    preview: &SummaryPreview,
    blocked: Option<&str>,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    if let Some(reason) = blocked {
        lines.extend(blocked_warning_lines(reason, theme));
    }
    lines.push(Line::from(""));

    let nothing_to_do = preview.stats.moves_total == 0
        && preview.stats.copies_total == 0
        && preview.stats.file_orphans == 0;
    if nothing_to_do {
        lines.push(Line::from(Span::styled(
            format!(
                "Nothing to organize — {} already in place.",
                pluralize(preview.stats.skipped, "file", "files")
            ),
            Style::default().fg(theme.fg_muted),
        )));
        return lines;
    }

    if preview.stats.moves_total > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                " {} to move:",
                pluralize(preview.stats.moves_total, "file", "files")
            ),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD),
        )));
        // Count is shown BEFORE the path ("5×  /path") to avoid the trailing
        // "(N)" reading as part of the directory name.
        for g in &preview.dir_moves {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {:>3}× ", g.count),
                    Style::default().fg(theme.fg_muted),
                ),
                Span::styled(g.from_dir.clone(), Style::default().fg(theme.fg_dim)),
            ]));
            lines.push(Line::from(Span::styled(
                format!("        → {}", g.to_dir),
                Style::default().fg(theme.accent),
            )));
        }
        if !preview.in_place_renames.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " renamed in place:",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            )));
            for g in &preview.in_place_renames {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("   {:>3}× ", g.count),
                        Style::default().fg(theme.fg_muted),
                    ),
                    Span::styled(g.dir.clone(), Style::default().fg(theme.fg_dim)),
                ]));
            }
        }
    }

    if preview.stats.copies_total > 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                " {} to copy into collections:",
                pluralize(preview.stats.copies_total, "file", "files")
            ),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD),
        )));
        for g in &preview.collection_copies {
            lines.push(Line::from(Span::styled(
                format!(
                    "   {} ({})",
                    g.collection_name,
                    pluralize(g.count, "file", "files")
                ),
                Style::default().fg(theme.accent_alt),
            )));
        }
    }

    // Absorbed orphans are reported as an inline note under the moves
    // section — the files aren't going anywhere new, a move is just
    // overwriting them with the freshly-imported version.
    if preview.stats.file_orphans_absorbed > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "   ({} of these overwrite existing files from prior dup-replace)",
                preview.stats.file_orphans_absorbed
            ),
            Style::default().fg(theme.fg_muted).add_modifier(Modifier::ITALIC),
        )));
    }

    if preview.stats.file_orphans > 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                " {} dangling file(s) from prior dup-replace (will be deleted):",
                preview.stats.file_orphans
            ),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "   these have no replacement — see Details view for paths",
            Style::default().fg(theme.fg_muted),
        )));
    }

    lines
}

fn build_detail_lines<'a>(
    preview: &DetailPreview,
    blocked: Option<&str>,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    if let Some(reason) = blocked {
        lines.extend(blocked_warning_lines(reason, theme));
    }

    let nothing_to_do = preview.stats.moves_total == 0
        && preview.stats.copies_total == 0
        && preview.stats.orphans == 0
        && preview.stats.file_orphans == 0;
    if nothing_to_do {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Nothing to organize — {} already in place.",
                pluralize(preview.stats.skipped, "file", "files")
            ),
            Style::default().fg(theme.fg_muted),
        )));
        return lines;
    }

    if preview.stats.moves_total > 0 {
        lines.push(Line::from(Span::styled(
            format!(" Moves ({}):", preview.stats.moves_total),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        lines.push(Line::from(""));
        for m in &preview.moves {
            let mut header_spans = vec![
                Span::styled("  ", Style::default()),
                Span::styled(m.from_name.clone(), Style::default().fg(theme.fg)),
            ];
            if m.overwrites_orphan {
                // Inline tag so the user sees on the same line that this
                // move is the "keep new" replacement from a prior import,
                // not a fresh add.
                header_spans.push(Span::styled(
                    "  ⟲ overwrites existing",
                    Style::default()
                        .fg(theme.yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(header_spans));
            lines.push(Line::from(vec![
                Span::styled("    from: ", Style::default().fg(theme.fg_muted)),
                Span::styled(m.from_dir.clone(), Style::default().fg(theme.fg_dim)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("    → to: ", Style::default().fg(theme.fg_muted)),
                Span::styled(m.to_dir.clone(), Style::default().fg(theme.accent)),
                Span::styled(
                    if m.renamed {
                        format!("/{}", m.to_name)
                    } else {
                        String::new()
                    },
                    Style::default().fg(theme.yellow),
                ),
            ]));
            if m.overwrites_orphan {
                lines.push(Line::from(vec![
                    Span::styled("    note: ", Style::default().fg(theme.fg_muted)),
                    Span::styled(
                        "replaces a file logged for cleanup during a prior dup-replace import",
                        Style::default().fg(theme.fg_dim),
                    ),
                ]));
            }
            // Note: `m.also_collection` is only set when the move destination
            // is already inside that collection's folder, so surfacing it here
            // reads as redundant — it's used downstream by apply_organize to
            // update collection_tracks.collection_file_path.
            lines.push(Line::from(""));
        }
    }

    if preview.stats.copies_total > 0 {
        lines.push(Line::from(Span::styled(
            format!(" Collection copies ({}):", preview.stats.copies_total),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        lines.push(Line::from(""));
        for c in &preview.copies {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", c.name), Style::default().fg(theme.fg)),
                Span::styled(
                    format!("→ {}", c.to_dir),
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

    if preview.stats.orphans > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                " Orphaned tracks ({} — {}):",
                preview.stats.orphans,
                if blocked.is_some() {
                    "prune blocked, rows kept"
                } else {
                    "will be pruned"
                }
            ),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for o in &preview.orphans {
            lines.push(Line::from(Span::styled(
                format!("  [{}] {} — {}", o.id, o.title, o.path.display()),
                Style::default().fg(theme.fg_dim),
            )));
        }
        lines.push(Line::from(""));
    }

    if preview.stats.file_orphans > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                " Dangling orphan files ({} — will be deleted):",
                preview.stats.file_orphans
            ),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        lines.push(Line::from(Span::styled(
            "  (These have no replacement coming in — they will cease to exist.",
            Style::default().fg(theme.fg_muted),
        )));
        lines.push(Line::from(Span::styled(
            "   Orphans that will be overwritten by a move are listed above.)",
            Style::default().fg(theme.fg_muted),
        )));
        lines.push(Line::from(""));
        for f in &preview.file_orphans {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(f.label.clone(), Style::default().fg(theme.fg)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("    path: ", Style::default().fg(theme.fg_muted)),
                Span::styled(
                    f.path.display().to_string(),
                    Style::default().fg(theme.fg_dim),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("    why:  ", Style::default().fg(theme.fg_muted)),
                Span::styled(f.reason.clone(), Style::default().fg(theme.fg_dim)),
            ]));
            lines.push(Line::from(""));
        }
    }

    lines
}

