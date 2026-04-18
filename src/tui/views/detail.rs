use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::config::Settings;
use crate::core::organizer::{self, OrganizePlan};
use crate::db::queries::{self, AlbumRow, TrackRow};
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::views::library::format_duration_ms;
use crate::tui::widgets::add_to_collection::{AddToCollectionPopup, PopupAction};
use crate::tui::widgets::input::TextInput;

pub enum DetailAction {
    None,
    EditTrack(i64),
    Organize,
}

#[derive(Clone, Copy)]
pub enum NoticeKind {
    Success,
    Warning,
}

#[derive(Default)]
pub struct AlbumDetailView {
    pub album: Option<AlbumRow>,
    pub tracks: Vec<TrackRow>,
    pub filter: String,
    pub selected: usize,
    pub scroll_offset: usize,
    rename_input: Option<TextInput>,
    add_to_collection: Option<AddToCollectionPopup>,
    organize_plan: Option<OrganizePlan>,
    organize_scroll: usize,
    organize_max_scroll: usize,
    organize_details: bool,
    notice: Option<(NoticeKind, String)>,
}

impl AlbumDetailView {
    /// Return indices of tracks matching the current filter.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.tracks.len()).collect();
        }
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let artist = t.artist.as_deref().unwrap_or("");
                crate::tui::fuzzy::matches_any(&self.filter, &[&t.title, artist])
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected = 0;
        self.scroll_offset = 0;
    }
}

impl AlbumDetailView {
    pub fn load(&mut self, conn: &Connection, album_id: i64) -> Result<()> {
        self.album = queries::get_album(conn, album_id)?;
        self.tracks = queries::get_album_tracks(conn, album_id)?;
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.rename_input = None;
        self.add_to_collection = None;
        self.notice = None;
        Ok(())
    }

    /// Load all loose tracks (no album). `album` is set to `None`, so the
    /// view renders as "Loose Tracks" and the rename binding is disabled.
    pub fn load_loose(&mut self, conn: &Connection) -> Result<()> {
        self.album = None;
        // Fetch enough loose tracks for the typical library size
        self.tracks = queries::list_loose_tracks(conn, 0, 5000)?;
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.rename_input = None;
        self.add_to_collection = None;
        self.notice = None;
        Ok(())
    }

    fn is_loose(&self) -> bool {
        self.album.is_none()
    }

    pub fn has_popup(&self) -> bool {
        self.rename_input.is_some()
            || self.add_to_collection.is_some()
            || self.organize_plan.is_some()
    }

    pub fn set_organize_plan(&mut self, plan: OrganizePlan) {
        self.organize_plan = Some(plan);
        self.organize_scroll = 0;
        self.organize_details = false;
    }

    pub fn set_notice(&mut self, kind: NoticeKind, msg: String) {
        self.notice = Some((kind, msg));
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        conn: &Connection,
        settings: &Settings,
    ) -> DetailAction {
        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                if let Some(plan) = self.organize_plan.take() {
                    match organizer::apply_organize(
                        conn,
                        &plan,
                        &settings.import.organize_operation,
                        &organizer::cleanup_roots(settings),
                    ) {
                        Ok(result) => {
                            let mut parts = Vec::new();
                            if result.moved > 0 {
                                parts.push(format!("{} moved", result.moved));
                            }
                            if result.copied > 0 {
                                parts.push(format!("{} copied", result.copied));
                            }
                            if result.dirs_cleaned > 0 {
                                parts.push(format!("{} dirs cleaned", result.dirs_cleaned));
                            }
                            let had_errors = !result.errors.is_empty();
                            if had_errors {
                                parts.push(format!("{} errors", result.errors.len()));
                            }
                            if !parts.is_empty() {
                                let kind = if had_errors {
                                    NoticeKind::Warning
                                } else {
                                    NoticeKind::Success
                                };
                                self.notice = Some((
                                    kind,
                                    format!("Organized: {}", parts.join(", ")),
                                ));
                            }
                        }
                        Err(e) => {
                            self.notice = Some((
                                NoticeKind::Warning,
                                format!("Organize failed: {}", e),
                            ));
                        }
                    }
                    // Reload to reflect new paths
                    if let Some(album) = &self.album {
                        let prev = self.selected;
                        self.load(conn, album.id).ok();
                        if prev < self.tracks.len() {
                            self.selected = prev;
                        }
                    }
                }
                return DetailAction::None;
            }
            if keys::is_back(&key) {
                if self.organize_details {
                    self.organize_details = false;
                    self.organize_scroll = 0;
                } else {
                    self.organize_plan = None;
                    self.organize_scroll = 0;
                }
                return DetailAction::None;
            }
            if key.code == KeyCode::Char('d') {
                self.organize_details = !self.organize_details;
                self.organize_scroll = 0;
                return DetailAction::None;
            }
            // Scroll the plan list (content is pinned under a footer in the popup)
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
            return DetailAction::None;
        }

        // Add-to-collection popup captures input
        if self.add_to_collection.is_some() {
            return self.handle_add_to_collection_key(key, conn);
        }

        // Rename mode captures input
        if let Some(input) = &mut self.rename_input {
            if keys::is_back(&key) {
                self.rename_input = None;
                return DetailAction::None;
            }
            if keys::is_confirm(&key) {
                let new_title = input.value.trim().to_string();
                if let Some(album) = &self.album
                    && !new_title.is_empty() && new_title != album.title {
                        let id = album.id;
                        queries::rename_album(conn, id, &new_title).ok();
                        // Reload to reflect the change
                        self.load(conn, id).ok();
                        return DetailAction::None;
                    }
                self.rename_input = None;
                return DetailAction::None;
            }
            input.handle_key(key);
            return DetailAction::None;
        }

        let visible = self.filtered_indices();
        let count = visible.len();

        if keys::is_up(&key) && self.selected > 0 {
            self.selected -= 1;
        }
        if keys::is_down(&key) && count > 0 && self.selected < count - 1 {
            self.selected += 1;
        }
        if keys::is_page_up(&key) {
            self.selected = self.selected.saturating_sub(20);
        }
        if keys::is_page_down(&key) && count > 0 {
            self.selected = (self.selected + 20).min(count - 1);
        }
        if keys::is_half_page_up(&key) {
            self.selected = self.selected.saturating_sub(10);
        }
        if keys::is_half_page_down(&key) && count > 0 {
            self.selected = (self.selected + 10).min(count - 1);
        }
        if key.code == KeyCode::Char('G') && count > 0 {
            self.selected = count - 1;
        }

        let current_track = visible
            .get(self.selected)
            .and_then(|&i| self.tracks.get(i));

        if key.code == KeyCode::Char('e')
            && let Some(track) = current_track {
                return DetailAction::EditTrack(track.id);
            }
        if key.code == KeyCode::Char('a')
            && let Some(track) = current_track {
                self.add_to_collection = Some(AddToCollectionPopup::open(
                    vec![track.id],
                    track.title.clone(),
                    conn,
                ));
            }
        if key.code == KeyCode::Char('o')
            && let Some(track) = current_track {
                let path = std::path::Path::new(&track.file_path);
                if let Some(parent) = path.parent() {
                    if !parent.exists() {
                        self.notice = Some((
                            NoticeKind::Warning,
                            format!("Directory not found: {}", parent.display()),
                        ));
                    } else if !open_directory(parent) {
                        self.notice = Some((
                            NoticeKind::Warning,
                            format!(
                                "Could not open file manager — path: {}",
                                parent.display()
                            ),
                        ));
                    }
                }
            }
        if key.code == KeyCode::Char('R') && !self.is_loose() {
            // Rename album
            if let Some(album) = &self.album {
                let mut input =
                    TextInput::new("New album title...").with_label(" Title: ");
                input.value = album.title.clone();
                input.cursor = input.value.len();
                input.focused = true;
                self.rename_input = Some(input);
            }
        }

        if key.code == KeyCode::Char('O') {
            return DetailAction::Organize;
        }

        DetailAction::None
    }

    fn handle_add_to_collection_key(
        &mut self,
        key: KeyEvent,
        conn: &Connection,
    ) -> DetailAction {
        let popup = match self.add_to_collection.as_mut() {
            Some(p) => p,
            None => return DetailAction::None,
        };
        match popup.handle_key(key, conn) {
            PopupAction::None => {}
            PopupAction::Closed(notice) => {
                self.add_to_collection = None;
                if let Some(n) = notice
                    && !n.is_empty() {
                        self.notice = Some((NoticeKind::Success, n));
                    }
            }
        }
        DetailAction::None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // album header
                Constraint::Min(5),   // track table
                Constraint::Length(2), // metadata
            ])
            .split(area);

        // Header
        let header = if let Some(album) = &self.album {
            let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
            let year = album
                .year
                .map(|y| format!(" ({})", y))
                .unwrap_or_default();
            Line::from(vec![
                Span::styled(
                    format!(" {} ", artist),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("— ", Style::default().fg(theme.fg_muted)),
                Span::styled(
                    format!("{}{}", album.title, year),
                    Style::default()
                        .fg(theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
        } else {
            // Loose tracks view
            Line::from(vec![
                Span::styled(
                    " Loose Tracks ",
                    Style::default()
                        .fg(theme.accent_alt)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("· {} tracks (not in any album)", self.tracks.len()),
                    Style::default().fg(theme.fg_dim),
                ),
            ])
        };
        let p = Paragraph::new(header).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme.border)),
        );
        frame.render_widget(p, chunks[0]);

        // Track table — iterate over filtered indices
        let visible = self.filtered_indices();
        let total = visible.len();
        let visible_height = chunks[1].height.saturating_sub(1) as usize;
        let scroll = if self.selected < self.scroll_offset {
            self.selected
        } else if self.selected + 1 >= self.scroll_offset + visible_height {
            (self.selected + 2).saturating_sub(visible_height)
        } else {
            self.scroll_offset
        };

        let mut rows = Vec::new();
        for pos in scroll..total.min(scroll + visible_height) {
            let i = visible[pos];
            let track = &self.tracks[i];
            let is_selected = pos == self.selected;

            let num = track
                .track_number
                .map(|n| format!("{:>2}", n))
                .unwrap_or_else(|| "  ".to_string());
            let duration = track
                .duration_ms
                .map(|ms| {
                    let s = ms / 1000;
                    format!("{}:{:02}", s / 60, s % 60)
                })
                .unwrap_or_default();
            let bitrate = track
                .bitrate
                .map(|b| format!("{} kbps", b))
                .unwrap_or_default();

            let status_style = match track.tag_status.as_str() {
                "verified" => Style::default().fg(theme.green),
                "matched" => Style::default().fg(theme.cyan),
                "manual" => Style::default().fg(theme.yellow),
                _ => Style::default().fg(theme.fg_muted),
            };

            let row = Row::new(vec![
                Cell::from(num),
                Cell::from(track.title.clone()),
                Cell::from(duration),
                Cell::from(Span::styled(track.tag_status.clone(), status_style)),
                Cell::from(bitrate),
            ]);

            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD)
            } else if pos % 2 == 0 {
                Style::default().bg(theme.bg).fg(theme.fg)
            } else {
                Style::default().bg(theme.bg_alt).fg(theme.fg)
            };

            rows.push(row.style(style));
        }

        let header = Row::new(vec![
            Cell::from(Span::styled("#", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Title", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Duration", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Status", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Bitrate", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Percentage(45),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .header(header);

        frame.render_widget(table, chunks[1]);

        // Metadata footer: line 1 = album info, line 2 = selected track path
        // (or notice if one is active)
        let footer_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[2]);

        // Line 1: album metadata or notice
        if let Some((kind, msg)) = &self.notice {
            let color = match kind {
                NoticeKind::Success => theme.green,
                NoticeKind::Warning => theme.yellow,
            };
            let p = Paragraph::new(Span::styled(
                format!(" {} ", msg),
                Style::default().fg(color),
            ))
            .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, footer_chunks[0]);
        } else if let Some(album) = &self.album {
            let fmt = album.formats.to_uppercase();
            let duration = format_duration_ms(album.total_duration_ms);
            let mut parts = vec![format!(" {} · {} tracks · {}", fmt, album.track_count, duration)];
            if let Some(label) = &album.label
                && !label.is_empty() {
                    parts.push(format!("Label: {}", label));
                }
            if let Some(genre) = &album.genre
                && !genre.is_empty() {
                    parts.push(genre.clone());
                }
            if let Some(mbid) = &album.mbid
                && !mbid.is_empty() {
                    parts.push(format!("MB: {}", &mbid[..mbid.len().min(8)]));
                }
            let meta = parts.join(" · ");
            let p = Paragraph::new(Span::styled(meta, Style::default().fg(theme.fg_dim)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, footer_chunks[0]);
        } else {
            let total_ms: i64 = self
                .tracks
                .iter()
                .map(|t| t.duration_ms.unwrap_or(0) as i64)
                .sum();
            let meta = format!(
                " {} loose track(s) · {}",
                self.tracks.len(),
                format_duration_ms(total_ms),
            );
            let p = Paragraph::new(Span::styled(meta, Style::default().fg(theme.fg_dim)))
                .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, footer_chunks[0]);
        }

        // Line 2: selected track's file path
        let selected_track = visible
            .get(self.selected)
            .and_then(|&i| self.tracks.get(i));
        if let Some(track) = selected_track {
            let p = Paragraph::new(Span::styled(
                format!(" {}", track.file_path),
                Style::default().fg(theme.fg_muted),
            ))
            .style(Style::default().bg(theme.bg_alt));
            frame.render_widget(p, footer_chunks[1]);
        }

        // Rename popup
        if let Some(input) = &self.rename_input {
            use crate::tui::widgets::popup;
            let content = vec![Line::from("")];
            let inner = popup::render_popup(frame, area, theme, "Rename Album", &content, 60, 5);
            input.render(frame, inner, theme);
        }

        // Add-to-collection popup
        if let Some(popup) = &self.add_to_collection {
            popup.render(frame, area, theme);
        }

        // Organize preview popup
        if let Some(plan) = &self.organize_plan {
            use crate::tui::widgets::organize_popup::{self, OrganizeView};
            let nothing = plan.moves.is_empty() && plan.copies.is_empty();
            let (view, title, hint, width) = if self.organize_details {
                (
                    OrganizeView::Details,
                    "Organize Preview — Details",
                    "Enter = apply · d = summary · j/k = scroll · Esc = back",
                    90,
                )
            } else if nothing {
                (
                    OrganizeView::Summary,
                    "Organize Preview",
                    "Esc = close",
                    80,
                )
            } else {
                (
                    OrganizeView::Summary,
                    "Organize Preview",
                    "Enter = apply · d = details · j/k = scroll · Esc = cancel",
                    80,
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
}

/// Open a directory in the system file manager.
/// Returns true if a file manager was successfully contacted.
///
/// On Linux, uses the D-Bus `org.freedesktop.FileManager1` interface first
/// (works even from terminal multiplexers like zellij/tmux where env vars
/// like DISPLAY/WAYLAND_DISPLAY aren't propagated). Falls back to spawning
/// common file managers directly.
/// On macOS, uses `open`.
pub fn open_directory(path: &std::path::Path) -> bool {
    let null = std::process::Stdio::null;

    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open")
            .arg(path)
            .stdout(null())
            .stderr(null())
            .stdin(null())
            .spawn()
            .is_ok();
    }

    #[cfg(not(target_os = "macos"))]
    {
        let has_dbus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some();
        let has_display = std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some();

        // No graphical session reachable (e.g. SSH without X forwarding) — bail
        // up-front so the caller can show a notice instead of silently
        // "spawning" a file manager that immediately dies.
        if !has_dbus && !has_display {
            return false;
        }

        // Try D-Bus FileManager1 interface (works from zellij/tmux on Wayland).
        // dbus-send exits non-zero if the bus isn't reachable or the service
        // isn't registered, so the status check is meaningful.
        if has_dbus {
            let uri = format!("file://{}", path.display());
            if std::process::Command::new("dbus-send")
                .args([
                    "--session",
                    "--print-reply",
                    "--type=method_call",
                    "--dest=org.freedesktop.FileManager1",
                    "/org/freedesktop/FileManager1",
                    "org.freedesktop.FileManager1.ShowFolders",
                    &format!("array:string:{}", uri),
                    "string:",
                ])
                .stdout(null())
                .stderr(null())
                .stdin(null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return true;
            }
        }

        // Fallback: spawn a file manager directly. Requires a display —
        // .spawn() returns Ok even if the child immediately exits, but
        // checking has_display avoids the obvious "no display" case.
        if has_display {
            for cmd in &["nautilus", "dolphin", "thunar", "nemo", "pcmanfm", "xdg-open"] {
                if std::process::Command::new(cmd)
                    .arg(path)
                    .stdout(null())
                    .stderr(null())
                    .stdin(null())
                    .spawn()
                    .is_ok()
                {
                    return true;
                }
            }
        }
        false
    }
}
