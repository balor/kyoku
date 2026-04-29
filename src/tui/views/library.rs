use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use unicode_width::UnicodeWidthStr;

use crate::config::Settings;
use crate::core::{organizer, pruner};
use crate::db::queries::{self, AlbumRow, AlbumSort};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::selection::Selection;
use crate::tui::themes::Theme;
use crate::tui::widgets::add_to_collection::{AddToCollectionPopup, PopupAction};
use crate::tui::widgets::confirm_delete::{ConfirmAction, ConfirmDelete};

pub enum LibraryAction {
    None,
    OpenAlbum(i64),
    OpenLoose,
    SortChanged,
    OrganizeAll,
    /// User confirmed a delete — caller should reload library data and refresh counts.
    Deleted,
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
    /// Last viewport-aware `max_scroll` reported by the organize-popup
    /// renderer. Used by `handle_key` to clamp scroll increments so pressing
    /// `j` at the bottom doesn't let the counter drift past the content.
    pub organize_max_scroll: usize,
    pub notice: Option<String>,
    /// Album ids the user has marked for batch actions (e.g. delete). Keyed
    /// by id so sort/filter don't lose the set.
    pub selection: Selection,
    /// Pending delete — built by `d` and consumed by the confirm popup.
    pub pending_delete: Option<(pruner::DeletePlan, ConfirmDelete)>,
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
            organize_max_scroll: 0,
            notice: None,
            selection: Selection::default(),
            pending_delete: None,
        }
    }
}

impl LibraryView {
    pub fn load(&mut self, conn: &Connection, search: Option<&str>) -> Result<()> {
        self.selection.clear();
        self.pending_delete = None;
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
        self.add_to_collection.is_some()
            || self.organize_plan.is_some()
            || self.pending_delete.is_some()
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        conn: &Connection,
        settings: &Settings,
    ) -> LibraryAction {
        // Delete-confirm popup captures input
        if let Some((plan, popup)) = &mut self.pending_delete {
            match popup.handle_key(key) {
                ConfirmAction::None => return LibraryAction::None,
                ConfirmAction::Cancel => {
                    self.pending_delete = None;
                    return LibraryAction::None;
                }
                ConfirmAction::Confirm { delete_files } => {
                    let cleanup = organizer::cleanup_roots(settings);
                    let plan = plan.clone();
                    self.pending_delete = None;
                    match pruner::apply_delete_plan(conn, &plan, delete_files, &cleanup) {
                        Ok(report) => {
                            let mut parts = Vec::new();
                            if report.albums_deleted > 0 {
                                parts.push(format!("{} album(s)", report.albums_deleted));
                            }
                            if report.tracks_deleted > 0 {
                                parts.push(format!("{} track(s)", report.tracks_deleted));
                            }
                            if report.files_deleted > 0 {
                                parts.push(format!("{} file(s)", report.files_deleted));
                            }
                            if report.dirs_cleaned > 0 {
                                parts.push(format!("{} dirs cleaned", report.dirs_cleaned));
                            }
                            let msg = if parts.is_empty() {
                                "Nothing to delete".to_string()
                            } else {
                                format!("Deleted: {}", parts.join(", "))
                            };
                            self.notice = Some(msg);
                        }
                        Err(e) => {
                            self.notice = Some(format!("Delete failed: {}", e));
                        }
                    }
                    self.selection.clear();
                    return LibraryAction::Deleted;
                }
            }
        }

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
            // Scrolling is available in both the summary and details views.
            let max = self.organize_max_scroll;
            if keys::is_down(&key) {
                self.organize_scroll = (self.organize_scroll + 1).min(max);
            }
            if keys::is_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(1);
            }
            if keys::is_page_down(&key) {
                self.organize_scroll = (self.organize_scroll + 20).min(max);
            }
            if keys::is_page_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(20);
            }
            if keys::is_half_page_down(&key) {
                self.organize_scroll = (self.organize_scroll + 10).min(max);
            }
            if keys::is_half_page_up(&key) {
                self.organize_scroll = self.organize_scroll.saturating_sub(10);
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

        if keys::is_back(&key) && !self.selection.is_empty() {
            self.selection.clear();
            return LibraryAction::None;
        }

        if keys::is_toggle_select(&key) {
            // Only albums can be marked; the [loose] virtual row is skipped.
            if self.selected < self.albums.len() {
                let id = self.albums[self.selected].id;
                self.selection.toggle(id);
                // Advance cursor for quick range-marking.
                if count > 0 && self.selected < count - 1 {
                    self.selected += 1;
                }
            }
            return LibraryAction::None;
        }

        if keys::is_delete(&key) {
            let ids: Vec<i64> = if self.selection.is_empty() {
                if self.selected < self.albums.len() {
                    vec![self.albums[self.selected].id]
                } else {
                    Vec::new()
                }
            } else {
                self.selection.ids()
            };
            if ids.is_empty() {
                return LibraryAction::None;
            }
            let cleanup = organizer::cleanup_roots(settings);
            match pruner::plan_delete_albums(conn, &ids, &cleanup) {
                Ok(plan) => {
                    if plan.is_empty() {
                        self.notice = Some("Nothing to delete".to_string());
                        return LibraryAction::None;
                    }
                    let popup = build_album_confirm(&plan, ids.len());
                    self.pending_delete = Some((plan, popup));
                }
                Err(e) => {
                    self.notice = Some(format!("Delete plan failed: {}", e));
                }
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
            self.sort_ascending = true;
            return LibraryAction::SortChanged;
        }

        if key.code == KeyCode::Char('S') {
            self.sort_ascending = !self.sort_ascending;
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

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
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

        // Symmetric scrolloff = 1: keep 1 row visible above and below the
        // cursor whenever there's content to show in those slots. Persist
        // the result so navigation stays sticky frame-to-frame.
        self.scroll_offset =
            compute_scroll_offset(self.selected, self.scroll_offset, visible_height);
        let scroll_offset = self.scroll_offset;

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
                let gutter_span = if self.selection.contains(album.id) {
                    Span::styled("▎", Style::default().fg(theme.accent))
                } else {
                    Span::raw(" ")
                };

                Row::new(vec![
                    Cell::from(gutter_span),
                    Cell::from(truncate_str(artist, 24)),
                    Cell::from(truncate_str(&album.title, 30)),
                    Cell::from(year),
                    Cell::from(format!("{:>6}", album.track_count)),
                    Cell::from(fmt),
                ])
            } else {
                // [loose] entry
                Row::new(vec![
                    Cell::from(" "),
                    Cell::from(Span::styled(
                        "[loose]",
                        Style::default().fg(theme.fg_muted),
                    )),
                    Cell::from(Span::styled(
                        format!("{} unalbumed tracks", self.loose_count),
                        Style::default().fg(theme.fg_muted),
                    )),
                    Cell::from(""),
                    Cell::from(format!("{:>6}", self.loose_count)),
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

        // Mark the active sort column directly in its header instead of
        // burning a screen row on a `[sort: ...]` indicator. ▲ = ascending,
        // ▼ = descending. The "Fmt" column is not sortable.
        let arrow = if self.sort_ascending { "▲" } else { "▼" };
        let header_cell = |label: &str, sort_for: Option<AlbumSort>| {
            let active = sort_for == Some(self.sort);
            if active {
                Cell::from(Span::styled(
                    format!("{} {}", label, arrow),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ))
            } else {
                Cell::from(Span::styled(
                    label.to_string(),
                    Style::default().fg(theme.accent),
                ))
            }
        };
        let header = Row::new(vec![
            Cell::from(" "),
            header_cell("Artist", Some(AlbumSort::Artist)),
            header_cell("Album", Some(AlbumSort::Album)),
            header_cell("Year", Some(AlbumSort::Year)),
            header_cell("Tracks", Some(AlbumSort::TrackCount)),
            header_cell("Fmt", None),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(0);

        let table = Table::new(
            rows,
            [
                ratatui::layout::Constraint::Length(1),
                ratatui::layout::Constraint::Percentage(25),
                ratatui::layout::Constraint::Percentage(30),
                ratatui::layout::Constraint::Length(6),
                ratatui::layout::Constraint::Length(8),
                ratatui::layout::Constraint::Length(6),
            ],
        )
        .header(header);

        frame.render_widget(table, area);

        // Add-to-collection popup overlay
        if let Some(popup) = &self.add_to_collection {
            popup.render(frame, area, theme);
        }

        // Delete-confirm popup overlay
        if let Some((_, popup)) = &self.pending_delete {
            popup.render(frame, area, theme);
        }

        // Organize preview popup
        if let Some(plan) = &self.organize_plan {
            use crate::tui::widgets::organize_popup::{self, OrganizeView};
            let (view, title, hint, width) = if self.organize_details {
                (
                    OrganizeView::Details,
                    "Organize Library — Details",
                    "Enter = apply · d = summary · j/k = scroll · Esc = back",
                    90,
                )
            } else {
                (
                    OrganizeView::Summary,
                    "Organize Library",
                    "Enter = apply · d = details · j/k = scroll · Esc = cancel",
                    85,
                )
            };
            self.organize_max_scroll = organize_popup::render(
                frame,
                area,
                theme,
                plan,
                &mut self.organize_scroll,
                view,
                title,
                hint,
                width,
            );
        }
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

fn build_album_confirm(plan: &pruner::DeletePlan, album_count: usize) -> ConfirmDelete {
    let primary = if album_count == 1 {
        "Delete 1 album?".to_string()
    } else {
        format!("Delete {} albums?", album_count)
    };
    let summary = format!(
        "{} track(s), {} file(s) on disk",
        plan.track_ids.len(),
        plan.deletable_file_count(),
    );
    let mut popup = ConfirmDelete::new("Confirm delete", primary).with_summary(summary);
    for line in &plan.album_summary_lines {
        popup = popup.with_detail(line.clone());
    }
    if plan.additional_albums > 0 {
        popup = popup.with_detail(format!("…and {} more album(s)", plan.additional_albums));
    }
    if !plan.files_outside_managed.is_empty() {
        popup = popup.with_warning(format!(
            "{} file(s) outside managed roots will NOT be deleted",
            plan.files_outside_managed.len()
        ));
    }
    if plan.deletable_file_count() == 0 {
        popup = popup.without_checkbox();
    } else {
        popup = popup.with_checkbox_label(format!(
            "Also delete {} file(s) from disk",
            plan.deletable_file_count()
        ));
    }
    popup
}

/// Compute a viewport offset that keeps `selected` inside the visible window
/// with a 1-row scrolloff on both sides (cursor is never on the very first or
/// very last visible row when there is content beyond it).
pub fn compute_scroll_offset(selected: usize, current: usize, visible_height: usize) -> usize {
    if visible_height < 3 {
        // Too cramped to honor scrolloff — fall back to plain clamping.
        if selected < current {
            return selected;
        }
        if selected >= current + visible_height {
            return (selected + 1).saturating_sub(visible_height);
        }
        return current;
    }
    if selected == 0 {
        return 0;
    }
    if selected <= current {
        // Cursor at top of viewport — pull offset up so 1 row stays visible above.
        return selected - 1;
    }
    if selected + 1 >= current + visible_height {
        // Cursor at (or past) bottom of viewport — push offset down so 1 row
        // stays visible below.
        return (selected + 2).saturating_sub(visible_height);
    }
    current
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
