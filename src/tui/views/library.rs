use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use unicode_width::UnicodeWidthStr;

use crate::db::queries::{self, AlbumRow, AlbumSort};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::add_to_collection::{AddToCollectionPopup, PopupAction};

pub enum LibraryAction {
    None,
    OpenAlbum(i64),
    OpenLoose,
    SortChanged,
    OrganizeAll,
}

pub struct LibraryView {
    pub albums: Vec<AlbumRow>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub total_albums: usize,
    pub loose_count: i64,
    pub sort: AlbumSort,
    pub sort_ascending: bool,
    pub add_to_collection: Option<AddToCollectionPopup>,
    pub organize_plan: Option<crate::core::organizer::OrganizePlan>,
    pub organize_details: bool,
    pub organize_scroll: usize,
    pub notice: Option<String>,
}

impl Default for LibraryView {
    fn default() -> Self {
        Self {
            albums: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            total_albums: 0,
            loose_count: 0,
            sort: AlbumSort::Artist,
            sort_ascending: true,
            add_to_collection: None,
            organize_plan: None,
            organize_details: false,
            organize_scroll: 0,
            notice: None,
        }
    }
}

impl LibraryView {
    pub fn load(&mut self, conn: &Connection, search: Option<&str>) -> Result<()> {
        if let Some(query) = search
            && !query.is_empty() {
                self.albums = queries::search_albums(conn, query, 500)?;
                self.total_albums = self.albums.len();
                self.loose_count = 0;
                self.clamp_selection();
                return Ok(());
            }
        self.albums = queries::list_albums(conn, self.sort, self.sort_ascending, 0, 500)?;
        self.total_albums = self.albums.len();
        self.loose_count = queries::count_loose_tracks(conn)?;
        self.clamp_selection();
        Ok(())
    }

    fn item_count(&self) -> usize {
        let base = self.albums.len();
        if self.loose_count > 0 {
            base + 1
        } else {
            base
        }
    }

    fn clamp_selection(&mut self) {
        let count = self.item_count();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    pub fn has_popup(&self) -> bool {
        self.add_to_collection.is_some() || self.organize_plan.is_some()
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) -> LibraryAction {
        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                // Apply is handled by the caller (app.rs) — return the action
                return LibraryAction::OrganizeAll;
            }
            if keys::is_back(&key) {
                if self.organize_details {
                    self.organize_details = false;
                    self.organize_scroll = 0;
                } else {
                    self.organize_plan = None;
                }
                return LibraryAction::None;
            }
            if key.code == KeyCode::Char('d') {
                self.organize_details = !self.organize_details;
                self.organize_scroll = 0;
                return LibraryAction::None;
            }
            // Scroll in detail view
            if self.organize_details {
                if keys::is_down(&key) {
                    self.organize_scroll += 1;
                }
                if keys::is_up(&key) {
                    self.organize_scroll = self.organize_scroll.saturating_sub(1);
                }
                if keys::is_page_down(&key) {
                    self.organize_scroll += 20;
                }
                if keys::is_page_up(&key) {
                    self.organize_scroll = self.organize_scroll.saturating_sub(20);
                }
                if keys::is_half_page_down(&key) {
                    self.organize_scroll += 10;
                }
                if keys::is_half_page_up(&key) {
                    self.organize_scroll = self.organize_scroll.saturating_sub(10);
                }
            }
            return LibraryAction::None;
        }

        // Add-to-collection popup captures input
        if let Some(popup) = &mut self.add_to_collection {
            match popup.handle_key(key, conn) {
                PopupAction::None => {}
                PopupAction::Closed(notice) => {
                    self.add_to_collection = None;
                    if let Some(n) = notice
                        && !n.is_empty() {
                            self.notice = Some(n);
                        }
                }
            }
            return LibraryAction::None;
        }

        let count = self.item_count();

        if keys::is_up(&key) {
            if self.selected > 0 {
                self.selected -= 1;
            }
            return LibraryAction::None;
        }

        if keys::is_down(&key) {
            if count > 0 && self.selected < count - 1 {
                self.selected += 1;
            }
            return LibraryAction::None;
        }

        if keys::is_page_up(&key) {
            self.selected = self.selected.saturating_sub(20);
            return LibraryAction::None;
        }

        if keys::is_page_down(&key) {
            if count > 0 {
                self.selected = (self.selected + 20).min(count - 1);
            }
            return LibraryAction::None;
        }

        if keys::is_half_page_up(&key) {
            self.selected = self.selected.saturating_sub(10);
            return LibraryAction::None;
        }

        if keys::is_half_page_down(&key) {
            if count > 0 {
                self.selected = (self.selected + 10).min(count - 1);
            }
            return LibraryAction::None;
        }

        if keys::is_confirm(&key) {
            if self.selected < self.albums.len() {
                return LibraryAction::OpenAlbum(self.albums[self.selected].id);
            }
            // [loose] virtual entry
            if self.loose_count > 0 {
                return LibraryAction::OpenLoose;
            }
            return LibraryAction::None;
        }

        if key.code == KeyCode::Char('s') {
            self.sort = self.sort.next();
            return LibraryAction::SortChanged;
        }

        if key.code == KeyCode::Char('G') {
            if count > 0 {
                self.selected = count - 1;
            }
            return LibraryAction::None;
        }

        if key.code == KeyCode::Char('O') {
            return LibraryAction::OrganizeAll;
        }

        if key.code == KeyCode::Char('a') {
            // Add whole album (all tracks of selected album) to a collection
            if self.selected < self.albums.len() {
                let album = &self.albums[self.selected];
                if let Ok(tracks) = queries::get_album_tracks(conn, album.id)
                    && !tracks.is_empty() {
                        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
                        let display =
                            format!("{} ({} tracks)", album.title, tracks.len());
                        self.add_to_collection =
                            Some(AddToCollectionPopup::open(ids, display, conn));
                    }
            }
            return LibraryAction::None;
        }

        LibraryAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if self.item_count() == 0 {
            let text = "No albums yet. Press 'i' to import.";
            let line_w = text.chars().count() as u16;
            let y = area.y + area.height / 2;
            let x = area.x + area.width.saturating_sub(line_w) / 2;
            let w = line_w.min(area.width);
            let centered = Rect::new(x, y, w, 1);
            let msg = Paragraph::new(Span::styled(
                text,
                Style::default().fg(theme.fg_muted),
            ))
            .alignment(Alignment::Center)
            .style(Style::default().bg(theme.bg));
            frame.render_widget(msg, centered);
            return;
        }

        let visible_height = area.height.saturating_sub(1) as usize; // -1 for header

        // Adjust scroll offset — keep 1 extra row visible below cursor for lookahead
        let scroll_offset = if self.selected < self.scroll_offset {
            self.selected
        } else if self.selected + 1 >= self.scroll_offset + visible_height {
            (self.selected + 2).saturating_sub(visible_height)
        } else {
            self.scroll_offset
        };

        // Build rows
        let mut rows: Vec<Row> = Vec::new();
        let count = self.item_count();

        for i in scroll_offset..count.min(scroll_offset + visible_height) {
            let is_selected = i == self.selected;

            let row = if i < self.albums.len() {
                let album = &self.albums[i];
                let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
                let year = album
                    .year
                    .map(|y| y.to_string())
                    .unwrap_or_default();
                let fmt = abbreviate_formats(&album.formats);

                Row::new(vec![
                    Cell::from(truncate_str(artist, 24)),
                    Cell::from(truncate_str(&album.title, 30)),
                    Cell::from(year),
                    Cell::from(format!("{:>4}", album.track_count)),
                    Cell::from(fmt),
                ])
            } else {
                // [loose] entry
                Row::new(vec![
                    Cell::from(Span::styled(
                        "[loose]",
                        Style::default().fg(theme.fg_muted),
                    )),
                    Cell::from(Span::styled(
                        format!("{} unalbumed tracks", self.loose_count),
                        Style::default().fg(theme.fg_muted),
                    )),
                    Cell::from(""),
                    Cell::from(format!("{:>4}", self.loose_count)),
                    Cell::from("mix"),
                ])
            };

            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD)
            } else if i % 2 == 0 {
                Style::default().bg(theme.bg).fg(theme.fg)
            } else {
                Style::default().bg(theme.bg_alt).fg(theme.fg)
            };

            rows.push(row.style(style));
        }

        let header = Row::new(vec![
            Cell::from(Span::styled("Artist", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Album", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Year", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Tracks", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Fmt", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(0);

        let sort_indicator = format!(" [sort: {}] ", self.sort.label());

        let table = Table::new(
            rows,
            [
                ratatui::layout::Constraint::Percentage(25),
                ratatui::layout::Constraint::Percentage(30),
                ratatui::layout::Constraint::Length(6),
                ratatui::layout::Constraint::Length(6),
                ratatui::layout::Constraint::Length(6),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(theme.border))
                .title_bottom(Span::styled(
                    sort_indicator,
                    Style::default().fg(theme.fg_muted),
                )),
        );

        frame.render_widget(table, area);

        // Add-to-collection popup overlay
        if let Some(popup) = &self.add_to_collection {
            popup.render(frame, area, theme);
        }

        // Organize preview popup
        if let Some(plan) = &self.organize_plan {
            if self.organize_details {
                // Detail view: scrollable per-file listing
                self.render_organize_details(frame, area, theme, plan);
            } else {
                // Summary view: compact grouped overview
                self.render_organize_summary(frame, area, theme, plan);
            }
        }
    }

    fn render_organize_summary(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        plan: &crate::core::organizer::OrganizePlan,
    ) {
        use crate::tui::widgets::popup;

        let mut lines = vec![Line::from("")];

        if plan.moves.is_empty() && plan.copies.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(
                    "Nothing to organize — {} file(s) already in place.",
                    plan.skipped
                ),
                Style::default().fg(theme.fg_muted),
            )));
        } else {
            // Group moves by (source dir → target dir)
            let mut dir_groups: std::collections::BTreeMap<(String, String), usize> =
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
                *dir_groups.entry((from_dir, to_dir)).or_insert(0) += 1;
            }
            lines.push(Line::from(Span::styled(
                format!(" {} file(s) to move:", plan.moves.len()),
                Style::default()
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            )));
            for ((from_dir, to_dir), count) in dir_groups.iter().take(12) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("   {} ", from_dir),
                        Style::default().fg(theme.fg_dim),
                    ),
                    Span::styled(
                        format!("({}) ", count),
                        Style::default().fg(theme.fg_muted),
                    ),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("   → {}", to_dir),
                    Style::default().fg(theme.accent),
                )));
            }
            if dir_groups.len() > 12 {
                lines.push(Line::from(Span::styled(
                    format!("   … and {} more groups", dir_groups.len() - 12),
                    Style::default().fg(theme.fg_muted),
                )));
            }

            // Collection copies grouped by collection name
            if !plan.copies.is_empty() {
                let mut coll_counts: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for c in &plan.copies {
                    *coll_counts.entry(c.collection_name.clone()).or_insert(0) += 1;
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" {} collection copy/copies:", plan.copies.len()),
                    Style::default()
                        .fg(theme.fg)
                        .add_modifier(Modifier::BOLD),
                )));
                for (name, count) in &coll_counts {
                    lines.push(Line::from(Span::styled(
                        format!("   {} ({} files)", name, count),
                        Style::default().fg(theme.accent_alt),
                    )));
                }
            }
        }

        lines.push(Line::from(""));
        let mut stats = Vec::new();
        if plan.skipped > 0 {
            stats.push(format!("{} in place", plan.skipped));
        }
        if !plan.missing_sources.is_empty() {
            stats.push(format!("{} orphaned", plan.missing_sources.len()));
        }
        if !stats.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(" {}", stats.join(" · ")),
                Style::default().fg(theme.fg_muted),
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Enter = apply · d = show details · Esc = cancel",
            Style::default().fg(theme.fg_muted),
        )));

        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        popup::render_popup(frame, area, theme, "Organize Library", &lines, 85, height);
    }

    fn render_organize_details(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        plan: &crate::core::organizer::OrganizePlan,
    ) {
        use crate::tui::widgets::popup;

        // Build the full line list (could be very long)
        let mut all_lines: Vec<Line<'_>> = Vec::new();

        // Moves
        if !plan.moves.is_empty() {
            all_lines.push(Line::from(Span::styled(
                format!(" Moves ({}):", plan.moves.len()),
                Style::default()
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            all_lines.push(Line::from(""));
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

                all_lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(from_name, Style::default().fg(theme.fg)),
                ]));
                all_lines.push(Line::from(vec![
                    Span::styled("    from: ", Style::default().fg(theme.fg_muted)),
                    Span::styled(from_dir, Style::default().fg(theme.fg_dim)),
                ]));
                all_lines.push(Line::from(vec![
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
                if let Some((_, coll_name)) = &m.also_collection {
                    all_lines.push(Line::from(Span::styled(
                        format!("    (also collection: {})", coll_name),
                        Style::default().fg(theme.accent_alt),
                    )));
                }
                all_lines.push(Line::from(""));
            }
        }

        // Copies
        if !plan.copies.is_empty() {
            all_lines.push(Line::from(Span::styled(
                format!(" Collection copies ({}):", plan.copies.len()),
                Style::default()
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            all_lines.push(Line::from(""));
            for c in &plan.copies {
                let name = c.to.file_name().and_then(|f| f.to_str()).unwrap_or("?");
                let to_dir = c
                    .to
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                all_lines.push(Line::from(vec![
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
            all_lines.push(Line::from(""));
        }

        // Orphans
        if !plan.missing_sources.is_empty() {
            all_lines.push(Line::from(Span::styled(
                format!(
                    " Orphaned tracks ({} — will be pruned):",
                    plan.missing_sources.len()
                ),
                Style::default()
                    .fg(theme.yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for (id, path, title) in &plan.missing_sources {
                all_lines.push(Line::from(Span::styled(
                    format!("  [{}] {} — {}", id, title, path.display()),
                    Style::default().fg(theme.fg_dim),
                )));
            }
            all_lines.push(Line::from(""));
        }

        // Pinned footer lines (always visible — not scrolled with content)
        let stats_line = Line::from(Span::styled(
            format!(
                " {} move(s), {} copy/copies, {} in place",
                plan.moves.len(),
                plan.copies.len(),
                plan.skipped,
            ),
            Style::default().fg(theme.fg_muted),
        ));
        let hint_line = Line::from(Span::styled(
            "Enter = apply · d = summary · j/k = scroll · Esc = back",
            Style::default().fg(theme.fg_muted),
        ));
        let footer_height: u16 = 3; // separator + stats + hint

        // Pre-compute the content viewport so the title can show the page
        // indicator. inner = popup_height - 2 (borders); content = inner - footer.
        let popup_height = area.height.saturating_sub(4);
        let content_height = (popup_height as usize)
            .saturating_sub(2)
            .saturating_sub(footer_height as usize);
        let max_scroll = all_lines.len().saturating_sub(content_height);
        let scroll = self.organize_scroll.min(max_scroll);

        let inner = popup::render_popup(
            frame,
            area,
            theme,
            &format!(
                "Organize Library — Details [{}/{}]",
                scroll + 1,
                max_scroll + 1
            ),
            &[],
            90,
            popup_height,
        );

        // Split inner into scrollable content area + pinned footer
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Min(1),
                ratatui::layout::Constraint::Length(footer_height),
            ])
            .split(inner);

        let visible: Vec<Line<'_>> = all_lines
            .into_iter()
            .skip(scroll)
            .take(chunks[0].height as usize)
            .collect();

        let content_p = ratatui::widgets::Paragraph::new(visible)
            .style(Style::default().fg(theme.fg));
        frame.render_widget(content_p, chunks[0]);

        // Footer: separator + stats + hint
        let separator = Line::from(Span::styled(
            "─".repeat(chunks[1].width as usize),
            Style::default().fg(theme.border),
        ));
        let footer_p = ratatui::widgets::Paragraph::new(vec![separator, stats_line, hint_line])
            .style(Style::default().fg(theme.fg));
        frame.render_widget(footer_p, chunks[1]);
    }

    pub fn render_detail_bar(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if let Some(notice) = &self.notice {
            let p = Paragraph::new(Span::styled(
                format!(" {} ", notice),
                Style::default().fg(theme.green),
            ))
            .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
            return;
        }
        if self.selected < self.albums.len() {
            let album = &self.albums[self.selected];
            let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
            let year = album
                .year
                .map(|y| format!(" ({})", y))
                .unwrap_or_default();
            let fmt = abbreviate_formats(&album.formats);
            let duration = format_duration_ms(album.total_duration_ms);

            let detail = format!(
                " {} — {}{} · {} · {} tracks · {}",
                artist, album.title, year, fmt, album.track_count, duration
            );
            let p = Paragraph::new(Span::styled(detail, Style::default().fg(theme.fg_dim)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
        } else if self.loose_count > 0 {
            let detail = format!(" {} loose tracks (no album)", self.loose_count);
            let p = Paragraph::new(Span::styled(detail, Style::default().fg(theme.fg_muted)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, area);
        }
    }
}

fn truncate_str(s: &str, max_width: usize) -> String {
    let width = s.width();
    if width <= max_width {
        s.to_string()
    } else {
        let mut result = String::new();
        let mut w = 0;
        for c in s.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if w + cw > max_width.saturating_sub(1) {
                result.push('…');
                break;
            }
            result.push(c);
            w += cw;
        }
        result
    }
}

fn abbreviate_formats(formats: &str) -> String {
    let fmts: Vec<&str> = formats.split(',').collect();
    if fmts.len() == 1 {
        fmts[0].to_uppercase()
    } else {
        "mix".to_string()
    }
}

pub fn format_duration_ms(ms: i64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{}h {:02}m", hours, mins)
    } else {
        format!("{}:{:02}", mins, secs)
    }
}
