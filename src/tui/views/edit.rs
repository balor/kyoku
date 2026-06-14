//! Full tag editor.
//!
//! Replaces the earlier 4-field (title/artist/track/disc) DB-only editor
//! with a view that enumerates every standard-keyed frame on the track's
//! audio file via [`read_all_frames`], lets the user edit each one
//! inline, and on save routes changes to the file (via `write_tags`)
//! plus to the DB for the mirrored columns.
//!
//! Multi-value frames are shown joined with ` | ` and parsed back on
//! save — that preserves field multiplicity without adding extra UI.
//! Setting a value to empty deletes that frame; the underlying
//! `TagChanges` handling treats empty-set as `unset`.
//!
//! The `[tagging] write_tags` config toggles whether changes land on
//! disk. When disabled, the footer shows "DB only" and the editor skips
//! `write_tags()`, but still mirrors changes to the DB so the library
//! view reflects the edit.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::config::Settings;
use crate::core::tagger::{self, FrameEntry, FrameGroup, TagChanges, TagValue, display_name_for};
use crate::db::queries;
use crate::error::Result;
use crate::tui::keybindings as keys;
use crate::tui::themes::Theme;
use crate::tui::widgets::confirm_delete::ConfirmDelete;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::list_cursor::ListCursor;

/// Separator used to join / split multi-value frames in the text input.
/// Picked for visual distinctiveness; users aren't likely to type this
/// as part of a single value.
const MULTI_SEP: &str = " | ";

pub struct FrameRow {
    pub group: FrameGroup,
    pub key: lofty::tag::ItemKey,
    pub display_name: String,
    /// Original value(s) joined by `MULTI_SEP`. Empty string means the
    /// frame was not present on the file.
    pub original: String,
    /// User-edited value. Diverges from `original` once touched.
    pub current: String,
    /// Frame had more than one value when read. Preserved so save-time
    /// splitting picks multi-value handling.
    pub is_multi: bool,
}

impl FrameRow {
    pub fn is_modified(&self) -> bool {
        self.current != self.original
    }
}

pub struct EditorView {
    pub track_id: i64,
    pub track_title: String,
    pub file_path: PathBuf,
    pub frames: Vec<FrameRow>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub editing: bool,
    pub input: TextInput,
    pub notice: Option<String>,
    /// Snapshot of `[tagging] write_tags` at load time — dictates
    /// whether `save` touches disk or only the DB mirror.
    pub write_to_file: bool,
    /// True once a successful save has landed. Gates the "Saved" notice.
    pub saved: bool,
    /// Pending discard confirmation shown when leaving with unsaved edits.
    pub pending_discard: Option<ConfirmDelete>,
    /// Whether confirming the discard should quit the app instead of returning.
    pub discard_quit_on_confirm: bool,
}

impl Default for EditorView {
    fn default() -> Self {
        Self {
            track_id: 0,
            track_title: String::new(),
            file_path: PathBuf::new(),
            frames: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            editing: false,
            input: TextInput::new(""),
            notice: None,
            write_to_file: true,
            saved: false,
            pending_discard: None,
            discard_quit_on_confirm: false,
        }
    }
}

impl EditorView {
    pub fn load(&mut self, conn: &Connection, track_id: i64, settings: &Settings) -> Result<()> {
        self.track_id = track_id;
        self.selected = 0;
        self.scroll_offset = 0;
        self.editing = false;
        self.saved = false;
        self.notice = None;
        self.pending_discard = None;
        self.discard_quit_on_confirm = false;
        self.write_to_file = settings.tagging.write_tags;

        let Some(track) = queries::get_track(conn, &settings.library.music_dir, track_id)? else {
            self.frames.clear();
            return Ok(());
        };
        self.track_title = track.title.clone();
        self.file_path = PathBuf::from(&track.file_path);

        // Read all frames from the file. If the file is missing/unreadable,
        // fall back to the DB's known fields so the user can still do a
        // DB-only edit — not useless, since the library view pulls from DB.
        let frames_from_file = tagger::read_all_frames(&self.file_path).unwrap_or_default();

        self.frames = frames_from_file
            .into_iter()
            .map(frame_row_from_entry)
            .collect();

        // Ensure the core four mirrored fields always have a row even if
        // the file is missing or tag-less, so the editor is usable in that
        // degenerate case. We don't add a row when the frame is already
        // present from the file read.
        ensure_row(
            &mut self.frames,
            lofty::tag::ItemKey::TrackTitle,
            &track.title,
        );
        ensure_row(
            &mut self.frames,
            lofty::tag::ItemKey::TrackArtist,
            track.artist.as_deref().unwrap_or(""),
        );
        ensure_row(
            &mut self.frames,
            lofty::tag::ItemKey::TrackNumber,
            &track
                .track_number
                .map(|n| n.to_string())
                .unwrap_or_default(),
        );
        ensure_row(
            &mut self.frames,
            lofty::tag::ItemKey::DiscNumber,
            &track.disc_number.to_string(),
        );

        // Stable sort preserves the group-then-name order established by
        // `read_all_frames`, while placing any appended placeholder rows
        // into their correct buckets.
        self.frames.sort_by(|a, b| {
            a.group
                .sort_index()
                .cmp(&b.group.sort_index())
                .then_with(|| a.display_name.cmp(&b.display_name))
        });

        Ok(())
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn is_dirty(&self) -> bool {
        self.frames.iter().any(FrameRow::is_modified)
    }

    pub fn has_popup(&self) -> bool {
        self.editing || self.pending_discard.is_some()
    }

    pub fn request_discard_confirm(&mut self, quit_on_confirm: bool) {
        self.pending_discard = Some(
            ConfirmDelete::new("Discard changes", "Leave the tag editor without saving?")
                .with_summary("Unsaved tag edits will be lost.")
                .without_checkbox(),
        );
        self.discard_quit_on_confirm = quit_on_confirm;
    }

    pub fn handle_key(&mut self, key: KeyEvent, conn: &Connection) {
        if self.editing {
            if keys::is_back(&key) {
                // Cancel edit — discard the in-progress input.
                self.editing = false;
                return;
            }
            if keys::is_confirm(&key) {
                if let Some(row) = self.frames.get_mut(self.selected) {
                    row.current = self.input.value.clone();
                }
                self.editing = false;
                self.saved = false;
                self.notice = None;
                return;
            }
            self.input.handle_key(key);
            return;
        }

        if keys::is_save(&key) {
            self.save(conn);
            return;
        }

        if keys::is_confirm(&key) {
            if let Some(row) = self.frames.get(self.selected) {
                self.input = TextInput::new("").with_label("");
                self.input.value = row.current.clone();
                self.input.cursor = self.input.value.len();
                self.input.focused = true;
                self.editing = true;
            }
            return;
        }

        let mut cursor = ListCursor::new(self.selected, self.scroll_offset);
        if cursor.handle_key(&key, self.frames.len()) {
            self.selected = cursor.selected;
            self.scroll_offset = cursor.scroll;
        }
        if key.code == KeyCode::Tab && !self.frames.is_empty() {
            self.selected = (self.selected + 1) % self.frames.len();
        }
    }

    fn save(&mut self, conn: &Connection) {
        // Snapshot modified rows into owned data so we can mutate
        // `self.frames` later without holding an immutable borrow.
        let modified: Vec<(lofty::tag::ItemKey, String, bool)> = self
            .frames
            .iter()
            .filter(|f| f.is_modified())
            .map(|f| (f.key, f.current.clone(), f.is_multi))
            .collect();
        if modified.is_empty() {
            self.notice = Some("No changes to save.".to_string());
            return;
        }

        // Build the tag delta. Empty string → unset (remove the frame).
        let mut changes = TagChanges::default();
        for (key, current, is_multi) in &modified {
            if current.is_empty() {
                changes.unset.push(*key);
            } else if *is_multi {
                let parts: Vec<String> = current
                    .split(MULTI_SEP)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                changes.set.push((*key, TagValue::MultiText(parts)));
            } else {
                changes.set.push((*key, TagValue::Text(current.clone())));
            }
        }

        // File write, if enabled. The report's counts land in the save
        // notice so the user sees how many frames actually hit disk vs
        // how many were cleared.
        let file_report = if self.write_to_file {
            match tagger::write_tags(&self.file_path, &changes) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.notice = Some(format!("File write failed: {}", e));
                    return;
                }
            }
        } else {
            None
        };

        // DB mirror for the four tracks-table columns that `update_track_fields`
        // currently recognises.
        let mirror: Vec<(&'static str, String)> = modified
            .iter()
            .filter_map(|(key, value, _)| db_mirror_column(key).map(|col| (col, value.clone())))
            .collect();

        if !mirror.is_empty() {
            let refs: Vec<(&str, &str)> = mirror.iter().map(|(c, v)| (*c, v.as_str())).collect();
            if let Err(e) = queries::update_track_fields(conn, self.track_id, &refs) {
                self.notice = Some(format!("DB update failed: {}", e));
                return;
            }
        }

        // Promote current → original so further edits are diffed against
        // the freshly-saved state.
        for row in &mut self.frames {
            row.original = row.current.clone();
        }

        self.saved = true;
        let count = modified.len();
        self.notice = Some(match file_report {
            Some(r) => format!(
                "Saved {} field(s) — {} written, {} cleared (file + DB)",
                count, r.fields_written, r.fields_removed
            ),
            None => format!("Saved {} field(s) (DB only)", count),
        });
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Min(5),    // fields table
                Constraint::Length(1), // footer
            ])
            .split(area);

        self.render_header(frame, chunks[0], theme);
        self.render_frames_table(frame, chunks[1], theme);
        self.render_footer(frame, chunks[2], theme);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let header = Line::from(vec![
            Span::styled(" Editing: ", Style::default().fg(theme.fg_dim)),
            Span::styled(
                &self.track_title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let p = Paragraph::new(header).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme.border)),
        );
        frame.render_widget(p, area);
    }

    fn render_frames_table(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if self.frames.is_empty() {
            let p = Paragraph::new(Span::styled(
                " No tags found for this file.",
                Style::default().fg(theme.fg_muted),
            ));
            frame.render_widget(p, area);
            return;
        }

        // Keep the selected row inside the viewport with a 1-row scrolloff
        // on each side, so the cursor never sits flush against the top or
        // bottom of the visible window when content extends beyond it.
        let visible = area.height.saturating_sub(1) as usize; // -1 for table header
        self.scroll_offset = crate::tui::views::library::compute_scroll_offset(
            self.selected,
            self.scroll_offset,
            visible,
        );

        let mut rows: Vec<Row> = Vec::new();
        let mut last_group: Option<FrameGroup> = None;
        let visible_range = self.scroll_offset..self.frames.len().min(self.scroll_offset + visible);

        for i in visible_range.clone() {
            let row = &self.frames[i];
            let is_selected = i == self.selected;
            let show_group_header = last_group != Some(row.group);
            last_group = Some(row.group);

            let group_cell = if show_group_header {
                Cell::from(Span::styled(
                    row.group.label(),
                    Style::default()
                        .fg(theme.fg_muted)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Cell::from("")
            };

            let name_cell = Cell::from(Span::styled(
                &row.display_name,
                Style::default().fg(theme.fg_dim),
            ));

            // Value cell: show the current value; yellow+bold when modified;
            // when editing *this* row, the inline input overlay paints on
            // top (see after the table).
            let value_style = if row.is_modified() {
                Style::default()
                    .fg(theme.yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let value_display = if row.current.is_empty() {
                Span::styled("(empty)", Style::default().fg(theme.fg_muted))
            } else {
                Span::styled(row.current.as_str(), value_style)
            };
            let value_cell = Cell::from(value_display);

            let style = if is_selected {
                Style::default()
                    .bg(theme.bg_selected)
                    .add_modifier(Modifier::BOLD)
            } else if i % 2 == 0 {
                Style::default().bg(theme.bg).fg(theme.fg)
            } else {
                Style::default().bg(theme.bg_alt).fg(theme.fg)
            };

            rows.push(Row::new(vec![group_cell, name_cell, value_cell]).style(style));
        }

        let header = Row::new(vec![
            Cell::from(Span::styled("Group", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Field", Style::default().fg(theme.accent))),
            Cell::from(Span::styled("Value", Style::default().fg(theme.accent))),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(
            rows,
            [
                Constraint::Length(13),
                Constraint::Length(18),
                Constraint::Min(10),
            ],
        )
        .header(header);

        frame.render_widget(table, area);

        // Inline edit overlay — positioned over the value cell of the
        // selected row. The viewport model above guarantees the selected
        // row is visible. ratatui's Table inserts a 1-col gap between
        // each column by default, so value_col_x must include the two
        // gaps after the first two fixed-width columns.
        const GROUP_W: u16 = 13;
        const FIELD_W: u16 = 18;
        const COL_GAP: u16 = 1;
        if self.editing && self.selected >= self.scroll_offset {
            let row_in_view = self.selected - self.scroll_offset;
            let y = area.y + 1 + row_in_view as u16; // +1 for table header
            if y < area.y + area.height {
                let offset = GROUP_W + COL_GAP + FIELD_W + COL_GAP;
                let value_col_x = area.x + offset;
                let value_col_w = area.width.saturating_sub(offset);
                let input_area = Rect::new(value_col_x, y, value_col_w, 1);
                frame.render_widget(ratatui::widgets::Clear, input_area);
                self.input.render(frame, input_area, theme);
            }
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let badge = if self.write_to_file {
            Span::styled(" DB + file ", Style::default().fg(theme.green))
        } else {
            Span::styled(" DB only ", Style::default().fg(theme.fg_muted))
        };

        let dirty = self.frames.iter().any(|f| f.is_modified());
        let status: Span = if let Some(notice) = &self.notice {
            let color = if self.saved {
                theme.green
            } else if notice.contains("failed") {
                theme.red
            } else {
                theme.yellow
            };
            Span::styled(format!(" {}", notice), Style::default().fg(color))
        } else if dirty {
            Span::styled(
                " Unsaved changes. Ctrl+S to save, Esc to cancel.",
                Style::default().fg(theme.yellow),
            )
        } else {
            Span::styled(
                " Enter edits, Tab/j/k navigates, Esc leaves.",
                Style::default().fg(theme.fg_muted),
            )
        };

        let line = Line::from(vec![badge, Span::raw(" "), status]);
        let p = Paragraph::new(line).style(Style::default().bg(theme.bg_alt));
        frame.render_widget(p, area);
    }
}

fn frame_row_from_entry(e: FrameEntry) -> FrameRow {
    let is_multi = e.values.len() > 1;
    let joined = e.values.join(MULTI_SEP);
    FrameRow {
        group: e.group,
        key: e.key,
        display_name: e.display_name,
        original: joined.clone(),
        current: joined,
        is_multi,
    }
}

/// Append a placeholder row for `key` if no row for that key exists yet.
/// Used to guarantee the editor can still edit the core four mirrored
/// fields when the file tag set is empty or unreadable.
fn ensure_row(rows: &mut Vec<FrameRow>, key: lofty::tag::ItemKey, value: &str) {
    if rows.iter().any(|r| r.key == key) {
        return;
    }
    rows.push(FrameRow {
        group: FrameGroup::Standard,
        key,
        display_name: display_name_for(&key),
        original: value.to_string(),
        current: value.to_string(),
        is_multi: false,
    });
}

/// Map an `ItemKey` to its `tracks`-table column, for fields the DB
/// mirrors. Fields outside this set are file-only and rely on the next
/// library reload to re-import them from disk.
fn db_mirror_column(key: &lofty::tag::ItemKey) -> Option<&'static str> {
    use lofty::tag::ItemKey;
    match key {
        ItemKey::TrackTitle => Some("title"),
        ItemKey::TrackArtist => Some("artist"),
        ItemKey::TrackNumber => Some("track_number"),
        ItemKey::DiscNumber => Some("disc_number"),
        _ => None,
    }
}
