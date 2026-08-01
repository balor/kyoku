use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use rusqlite::Connection;

use crate::config::Settings;
use crate::core::organizer::{self, OrganizePlan};
use crate::core::pruner;
use crate::core::tagger::TMP_MARKER;
use crate::db::queries::{self, AlbumRow, TrackRow};
use crate::error::Result;
use crate::external::cover_art_archive::{CaaClient, CoverImage};
use crate::tui::keybindings as keys;
use crate::tui::selection::Selection;
use crate::tui::themes::Theme;
use crate::tui::views::library::format_duration_ms;
use crate::tui::widgets::add_to_collection::{AddToCollectionPopup, PopupAction};
use crate::tui::widgets::confirm_delete::{ConfirmAction, ConfirmDelete};
use crate::tui::widgets::cover_preview::CoverRegistry;
use crate::tui::widgets::input::TextInput;
use crate::tui::widgets::list_cursor::{self, ListCursor};
use crate::tui::widgets::track_table;

pub enum DetailAction {
    None,
    EditTrack(i64),
    Organize,
    /// User confirmed a delete — caller should reload and possibly go back.
    Deleted,
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
    pub selection: Selection,
    pending_delete: Option<(pruner::DeletePlan, ConfirmDelete)>,
    /// Active "overwrite existing cover?" prompt. Shown after the user
    /// presses `C` on an album that already has a cover file on disk;
    /// confirming kicks off the CAA fetch, cancelling is a no-op.
    pending_cover_overwrite: Option<ConfirmDelete>,
    /// Receiver for an in-flight CAA fetch. `Some` while the background
    /// thread is running; cleared once the result has been processed.
    /// Kept on the view (not on `App`) because the fetch is scoped to
    /// whichever album is currently shown — navigating away cancels the
    /// fetch implicitly by dropping the receiver.
    cover_fetch_rx: Option<mpsc::Receiver<CoverFetchResult>>,
}

/// Outcome of one background cover fetch. Always tagged with the album
/// id the fetch was initiated for, so a stale result from a previous
/// album doesn't get applied after the user navigates elsewhere.
struct CoverFetchResult {
    album_id: i64,
    result: crate::error::Result<Option<CoverImage>>,
}

impl AlbumDetailView {
    /// Return indices of tracks matching the current filter.
    pub fn filtered_indices(&self) -> Vec<usize> {
        list_cursor::filtered_indices(&self.tracks, &self.filter, |t| {
            let artist = t.artist.as_deref().unwrap_or("");
            crate::tui::fuzzy::matches_any(&self.filter, &[&t.title, artist])
        })
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected = 0;
        self.scroll_offset = 0;
    }
}

impl AlbumDetailView {
    pub fn load(&mut self, conn: &Connection, music_dir: &Path, album_id: i64) -> Result<()> {
        self.album = queries::get_album(conn, music_dir, album_id)?;
        self.tracks = queries::get_album_tracks(conn, music_dir, album_id)?;
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.rename_input = None;
        self.add_to_collection = None;
        self.notice = None;
        self.selection.clear();
        self.pending_delete = None;
        self.pending_cover_overwrite = None;
        // Dropping the receiver cancels any in-flight fetch scoped to the
        // previous album — the worker thread's send() will simply error
        // on the closed channel and the thread exits.
        self.cover_fetch_rx = None;
        Ok(())
    }

    /// Load all loose tracks (no album). `album` is set to `None`, so the
    /// view renders as "Loose Tracks" and the rename binding is disabled.
    pub fn load_loose(&mut self, conn: &Connection, music_dir: &Path) -> Result<()> {
        self.album = None;
        // Fetch enough loose tracks for the typical library size
        self.tracks = queries::list_loose_tracks(conn, music_dir, 0, 5000)?;
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.rename_input = None;
        self.add_to_collection = None;
        self.notice = None;
        self.selection.clear();
        self.pending_delete = None;
        self.pending_cover_overwrite = None;
        self.cover_fetch_rx = None;
        Ok(())
    }

    fn is_loose(&self) -> bool {
        self.album.is_none()
    }

    pub fn has_popup(&self) -> bool {
        self.rename_input.is_some()
            || self.add_to_collection.is_some()
            || self.organize_plan.is_some()
            || self.pending_delete.is_some()
            || self.pending_cover_overwrite.is_some()
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
        // Overwrite-cover confirm captures input first — it's a simple
        // y/n gate in front of the CAA fetch.
        if let Some(popup) = &mut self.pending_cover_overwrite {
            match popup.handle_key(key) {
                ConfirmAction::None => return DetailAction::None,
                ConfirmAction::Cancel => {
                    self.pending_cover_overwrite = None;
                    return DetailAction::None;
                }
                ConfirmAction::Confirm { .. } => {
                    self.pending_cover_overwrite = None;
                    self.launch_cover_fetch(settings);
                    return DetailAction::None;
                }
            }
        }

        // Delete-confirm popup captures input
        if let Some((plan, popup)) = &mut self.pending_delete {
            match popup.handle_key(key) {
                ConfirmAction::None => return DetailAction::None,
                ConfirmAction::Cancel => {
                    self.pending_delete = None;
                    return DetailAction::None;
                }
                ConfirmAction::Confirm { delete_files } => {
                    let file_delete_roots = organizer::file_delete_roots(settings);
                    let plan = plan.clone();
                    self.pending_delete = None;
                    match pruner::apply_delete_plan(
                        conn,
                        &settings.library.music_dir,
                        &plan,
                        delete_files,
                        &file_delete_roots,
                    ) {
                        Ok(report) => {
                            let mut parts = Vec::new();
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
                            self.notice = Some((NoticeKind::Success, msg));
                        }
                        Err(e) => {
                            self.notice =
                                Some((NoticeKind::Warning, format!("Delete failed: {}", e)));
                        }
                    }
                    self.selection.clear();
                    return DetailAction::Deleted;
                }
            }
        }

        // Organize popup captures input
        if self.organize_plan.is_some() {
            if keys::is_confirm(&key) {
                if let Some(plan) = self.organize_plan.take() {
                    match organizer::apply_organize(
                        conn,
                        &settings.library.music_dir,
                        &plan,
                        settings.import.organize_operation,
                        &organizer::cleanup_roots(settings),
                    ) {
                        Ok(result) => {
                            let mut parts = Vec::new();
                            let moved_total = result.moved + result.covers_moved;
                            if moved_total > 0 {
                                parts.push(format!("{} moved", moved_total));
                            }
                            if result.copied > 0 {
                                parts.push(format!("{} copied", result.copied));
                            }
                            if result.dirs_cleaned > 0 {
                                parts.push(format!("{} dirs cleaned", result.dirs_cleaned));
                            }
                            if result.file_orphans_removed > 0 {
                                parts.push(format!(
                                    "{} orphan files deleted",
                                    result.file_orphans_removed
                                ));
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
                                self.notice =
                                    Some((kind, format!("Organized: {}", parts.join(", "))));
                            }
                        }
                        Err(e) => {
                            self.notice =
                                Some((NoticeKind::Warning, format!("Organize failed: {}", e)));
                        }
                    }
                    // Reload to reflect new paths
                    if let Some(album) = &self.album {
                        let prev = self.selected;
                        self.load(conn, &settings.library.music_dir, album.id).ok();
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
                    && !new_title.is_empty()
                    && new_title != album.title
                {
                    let id = album.id;
                    match queries::rename_album(conn, id, &new_title) {
                        Ok(()) => {
                            // Reload to reflect the change
                            if let Err(e) = self.load(conn, &settings.library.music_dir, id) {
                                self.notice = Some((
                                    NoticeKind::Warning,
                                    format!("Album renamed, but reload failed: {e}"),
                                ));
                            }
                        }
                        Err(e) => {
                            self.notice =
                                Some((NoticeKind::Warning, format!("Rename failed: {e}")));
                        }
                    }
                    self.rename_input = None;
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

        let mut cursor = ListCursor::new(self.selected, self.scroll_offset);
        if cursor.handle_key(&key, count) {
            self.selected = cursor.selected;
            self.scroll_offset = cursor.scroll;
        }

        let current_track = visible.get(self.selected).and_then(|&i| self.tracks.get(i));

        if keys::is_back(&key) && !self.selection.is_empty() {
            self.selection.clear();
            return DetailAction::None;
        }

        if keys::is_toggle_select(&key)
            && let Some(track) = current_track
        {
            self.selection.toggle(track.id);
            if count > 0 && self.selected < count - 1 {
                self.selected += 1;
            }
            return DetailAction::None;
        }

        if keys::is_delete(&key) {
            let ids: Vec<i64> = if self.selection.is_empty() {
                current_track.map(|t| vec![t.id]).unwrap_or_default()
            } else {
                self.selection.ids()
            };
            if ids.is_empty() {
                return DetailAction::None;
            }
            let file_delete_roots = organizer::file_delete_roots(settings);
            match pruner::plan_delete_tracks(
                conn,
                &settings.library.music_dir,
                &ids,
                &file_delete_roots,
            ) {
                Ok(plan) => {
                    if plan.is_empty() {
                        self.notice = Some((NoticeKind::Warning, "Nothing to delete".to_string()));
                        return DetailAction::None;
                    }
                    let popup = build_track_confirm(&plan);
                    self.pending_delete = Some((plan, popup));
                }
                Err(e) => {
                    self.notice = Some((NoticeKind::Warning, format!("Delete plan failed: {}", e)));
                }
            }
            return DetailAction::None;
        }

        if key.code == KeyCode::Char('e')
            && let Some(track) = current_track
        {
            return DetailAction::EditTrack(track.id);
        }
        if key.code == KeyCode::Char('a')
            && let Some(track) = current_track
        {
            self.add_to_collection = Some(AddToCollectionPopup::open(
                vec![track.id],
                track.title.clone(),
                conn,
            ));
        }
        if key.code == KeyCode::Char('o')
            && let Some(track) = current_track
        {
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
                        format!("Could not open file manager — path: {}", parent.display()),
                    ));
                }
            }
        }
        if key.code == KeyCode::Char('R') && !self.is_loose() {
            // Rename album
            if let Some(album) = &self.album {
                let mut input = TextInput::new("New album title...").with_label(" Title: ");
                input.value = album.title.clone();
                input.cursor = input.value.len();
                input.focused = true;
                self.rename_input = Some(input);
            }
        }

        // p — play marked tracks (track-list order) or the track under
        // the cursor. P — play the whole album (or all loose tracks).
        if keys::is_play(&key) {
            let rows: Vec<TrackRow> = if !self.selection.is_empty() {
                self.tracks
                    .iter()
                    .filter(|t| self.selection.contains(t.id))
                    .cloned()
                    .collect()
            } else {
                current_track.cloned().into_iter().collect()
            };
            let context = if rows.len() == 1 {
                rows[0].title.clone()
            } else {
                format!("{} tracks", rows.len())
            };
            let items = crate::core::player::items_from_rows(&rows, None);
            match crate::core::player::play(settings, items) {
                Ok(outcome) => {
                    self.notice = Some((
                        NoticeKind::Success,
                        crate::core::player::outcome_notice(&outcome, &context),
                    ));
                }
                Err(e) => {
                    self.notice = Some((NoticeKind::Warning, format!("Play failed: {}", e)));
                }
            }
            return DetailAction::None;
        }
        if keys::is_play_scope(&key) {
            let music_dir = &settings.library.music_dir;
            let (items, context) = if let Some(album) = &self.album {
                (
                    crate::core::player::album_items(conn, music_dir, album.id).unwrap_or_default(),
                    album.title.clone(),
                )
            } else {
                let rows = queries::list_loose_tracks(conn, music_dir, 0, 5000).unwrap_or_default();
                (
                    crate::core::player::items_from_rows(&rows, None),
                    "loose tracks".to_string(),
                )
            };
            match crate::core::player::play(settings, items) {
                Ok(outcome) => {
                    self.notice = Some((
                        NoticeKind::Success,
                        crate::core::player::outcome_notice(&outcome, &context),
                    ));
                }
                Err(e) => {
                    self.notice = Some((NoticeKind::Warning, format!("Play failed: {}", e)));
                }
            }
            return DetailAction::None;
        }

        if key.code == KeyCode::Char('O') {
            return DetailAction::Organize;
        }

        if keys::is_fetch_cover(&key) {
            self.start_cover_fetch(settings);
            return DetailAction::None;
        }

        DetailAction::None
    }

    /// Entry point for the `C` key. Validates preconditions (no fetch
    /// already running, album has an MB release MBID) and either prompts
    /// for overwrite (cover already on disk) or launches the fetch
    /// straight away. The actual thread spawn lives in
    /// [`Self::launch_cover_fetch`] so the confirm path can call it too.
    fn start_cover_fetch(&mut self, settings: &Settings) {
        if self.cover_fetch_rx.is_some() || self.pending_cover_overwrite.is_some() {
            return;
        }
        let Some(album) = self.album.as_ref() else {
            return;
        };
        if album.mbid.as_deref().is_none_or(|s| s.is_empty()) {
            self.notice = Some((
                NoticeKind::Warning,
                "No MusicBrainz release ID — match this album first.".to_string(),
            ));
            return;
        }

        // If a cover already exists on disk, gate the fetch behind a
        // confirm — the download will overwrite the existing file and
        // the user may have placed a hand-picked cover there.
        let existing = album
            .cover_art_path
            .as_deref()
            .map(Path::new)
            .filter(|p| p.exists());
        if let Some(path) = existing {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("cover");
            self.pending_cover_overwrite = Some(
                ConfirmDelete::new(
                    "Overwrite cover",
                    format!("Replace existing cover ({})?", name),
                )
                .with_summary(
                    "A new cover will be downloaded from MusicBrainz and \
                     saved over the current file.",
                )
                .without_checkbox(),
            );
            return;
        }

        self.launch_cover_fetch(settings);
    }

    /// Spawn the CAA fetch on a background thread. Callers must have
    /// already validated that `album.mbid` is present — this is the
    /// path taken both on direct fetch (no existing cover) and on
    /// confirmed overwrite.
    fn launch_cover_fetch(&mut self, settings: &Settings) {
        let Some(album) = self.album.as_ref() else {
            return;
        };
        let Some(mbid) = album.mbid.clone().filter(|s| !s.is_empty()) else {
            return;
        };
        let album_id = album.id;
        let rate_limit_ms = settings.musicbrainz.rate_limit_ms;
        let size = settings.musicbrainz.cover_art_size;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut client = CaaClient::new(rate_limit_ms);
            let result = client.fetch_front(&mbid, size);
            // If the user navigated away the receiver is already dropped,
            // in which case send fails silently — nothing to do.
            let _ = tx.send(CoverFetchResult { album_id, result });
        });
        self.cover_fetch_rx = Some(rx);
        self.notice = Some((
            NoticeKind::Success,
            "Fetching cover art from MusicBrainz...".to_string(),
        ));
    }

    /// Called each tick — polls the cover-fetch receiver and, on a
    /// result, writes bytes to `<album_dir>/cover.<ext>`, updates
    /// `albums.cover_art_path`, and reloads the album row so the header
    /// picks up the new path on the next render.
    pub fn tick(&mut self, conn: &Connection, music_dir: &Path) {
        let Some(rx) = self.cover_fetch_rx.as_ref() else {
            return;
        };
        let Ok(msg) = rx.try_recv() else {
            return;
        };
        self.cover_fetch_rx = None;

        // Guard against stale results landing after navigation.
        let current_id = self.album.as_ref().map(|a| a.id);
        if current_id != Some(msg.album_id) {
            return;
        }

        match msg.result {
            Ok(Some(img)) => match self.save_fetched_cover(conn, music_dir, msg.album_id, &img) {
                Ok(path) => {
                    self.notice = Some((
                        NoticeKind::Success,
                        format!("Cover saved to {}", path.display()),
                    ));
                }
                Err(e) => {
                    self.notice = Some((NoticeKind::Warning, format!("Cover save failed: {}", e)));
                }
            },
            Ok(None) => {
                self.notice = Some((
                    NoticeKind::Warning,
                    "MusicBrainz has no cover art for this release.".to_string(),
                ));
            }
            Err(e) => {
                self.notice = Some((NoticeKind::Warning, format!("Cover fetch failed: {}", e)));
            }
        }
    }

    /// Write the fetched bytes to `<album_dir>/cover.<ext>`, record the
    /// path in the DB, and refresh the in-memory album row. The album
    /// directory is inferred from the first track's parent — safe because
    /// a post-organize album has all its tracks in one directory, and
    /// pre-organize the user presumably imports+organizes before fetching
    /// art anyway. Returns the saved path for the success notice.
    fn save_fetched_cover(
        &mut self,
        conn: &Connection,
        music_dir: &Path,
        album_id: i64,
        img: &CoverImage,
    ) -> Result<PathBuf> {
        let album_dir = self
            .tracks
            .first()
            .map(|t| PathBuf::from(&t.file_path))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .ok_or_else(|| {
                crate::error::KyokuError::External(
                    "album has no tracks on disk — can't infer a directory to save the cover in"
                        .to_string(),
                )
            })?;
        std::fs::create_dir_all(&album_dir)?;
        let dest = album_dir.join(format!("cover.{}", img.extension));
        let tmp = album_dir.join(format!("cover.{}{}", img.extension, TMP_MARKER));
        std::fs::write(&tmp, &img.bytes)?;
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        queries::set_album_cover_path(conn, music_dir, album_id, &dest.display().to_string())?;
        // Refresh the cached row so the header renders with the new path.
        self.album = queries::get_album(conn, music_dir, album_id)?;
        Ok(dest)
    }

    fn handle_add_to_collection_key(&mut self, key: KeyEvent, conn: &Connection) -> DetailAction {
        let popup = match self.add_to_collection.as_mut() {
            Some(p) => p,
            None => return DetailAction::None,
        };
        match popup.handle_key(key, conn) {
            PopupAction::None => {}
            PopupAction::Closed(notice) => {
                self.add_to_collection = None;
                if let Some(n) = notice
                    && !n.is_empty()
                {
                    self.notice = Some((NoticeKind::Success, n));
                }
            }
        }
        DetailAction::None
    }

    /// Draw the left-hand info panel: cover (or skeleton/no-cover
    /// tags) below. The cover slot is only reserved when the terminal
    /// can actually render the image (native graphics protocol
    /// available, a file exists on disk, and decode hasn't failed);
    /// otherwise the whole panel is text, with the cover filename
    /// surfaced there instead of as a pixel preview.
    fn render_info_panel(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        covers: &mut CoverRegistry,
        show_cover: bool,
        cover_path: Option<&Path>,
    ) {
        // Single-col left + single-row top gutter so the panel content
        // doesn't hug the edge of the terminal / view divider.
        let area = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(1),
            height: area.height.saturating_sub(1),
        };

        // Only reserve space for a real image. If the terminal can't
        // render one, or decode has already failed, or there's no cover
        // file at all, we drop the preview slot entirely — no fake-cover
        // tile, no skeleton-sized gap. The filename still surfaces in
        // the text block below.
        let show_preview = show_cover
            && covers.can_render_images()
            && cover_path.is_some_and(|p| !covers.has_failed(p));

        let (cover_area, text_area) = if show_preview {
            let cover_height = covers.square_cover_height(area.width);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(cover_height),
                    Constraint::Length(1), // spacer
                    Constraint::Min(0),
                ])
                .split(area);
            (Some(rows[0]), rows[2])
        } else {
            (None, area)
        };

        if let Some(cover_area) = cover_area
            && let Some(path) = cover_path
        {
            covers.render(frame, cover_area, theme, path);
        }

        self.render_info_text(frame, text_area, theme, cover_path);
    }

    /// Render the textual portion of the info panel (album title, artist,
    /// year, stats, tags). Kept separate so `render_info_panel` reads top
    /// to bottom without a wall of Line-building in the middle.
    ///
    /// `cover_path` is surfaced as a filename line in the Tags section —
    /// useful because on terminals without a native graphics protocol
    /// we don't render the preview tile, and the user still wants to
    /// know the cover exists (and what it's called).
    fn render_info_text(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        cover_path: Option<&Path>,
    ) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let mut lines: Vec<Line> = Vec::new();

        if let Some(album) = &self.album {
            // Title (bold, accent).
            lines.push(Line::from(Span::styled(
                album.title.clone(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )));

            // Artist · Year.
            let artist = album.album_artist.as_deref().unwrap_or("(unknown)");
            let year = album.year.map(|y| y.to_string()).unwrap_or_default();
            let artist_line = if year.is_empty() {
                Line::from(Span::styled(
                    artist.to_string(),
                    Style::default().fg(theme.fg),
                ))
            } else {
                Line::from(vec![
                    Span::styled(artist.to_string(), Style::default().fg(theme.fg)),
                    Span::styled(" · ", Style::default().fg(theme.fg_muted)),
                    Span::styled(year, Style::default().fg(theme.fg_dim)),
                ])
            };
            lines.push(artist_line);

            // Stats section.
            lines.push(Line::from(""));
            lines.push(section_heading("Stats", theme));
            lines.push(Line::from(Span::styled(
                format!("{} tracks", album.track_count),
                Style::default().fg(theme.fg_dim),
            )));
            lines.push(Line::from(Span::styled(
                format_duration_ms(album.total_duration_ms),
                Style::default().fg(theme.fg_dim),
            )));
            if !album.formats.is_empty() {
                lines.push(Line::from(Span::styled(
                    album.formats.to_uppercase(),
                    Style::default().fg(theme.fg_dim),
                )));
            }

            // Tags section — only emit when we actually have something.
            let has_label = album.label.as_deref().is_some_and(|s| !s.is_empty());
            let has_genre = album.genre.as_deref().is_some_and(|s| !s.is_empty());
            let has_mbid = album.mbid.as_deref().is_some_and(|s| !s.is_empty());
            let cover_name = cover_path
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str());
            if has_label || has_genre || has_mbid || cover_name.is_some() {
                lines.push(Line::from(""));
                lines.push(section_heading("Tags", theme));
                if let Some(label) = &album.label
                    && !label.is_empty()
                {
                    lines.push(tag_line("Label", label, theme));
                }
                if let Some(genre) = &album.genre
                    && !genre.is_empty()
                {
                    lines.push(tag_line("Genre", genre, theme));
                }
                if let Some(mbid) = &album.mbid
                    && !mbid.is_empty()
                {
                    let short: String = mbid.chars().take(8).collect();
                    lines.push(tag_line("MB", &short, theme));
                }
                if let Some(name) = cover_name {
                    lines.push(tag_line("Cover", name, theme));
                }
            }
        } else {
            // Loose tracks view — no album row, but we can still surface
            // basic stats so the panel isn't empty.
            lines.push(Line::from(Span::styled(
                "Loose Tracks",
                Style::default()
                    .fg(theme.accent_alt)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                "not in any album",
                Style::default().fg(theme.fg_dim),
            )));
            lines.push(Line::from(""));
            lines.push(section_heading("Stats", theme));
            lines.push(Line::from(Span::styled(
                format!("{} tracks", self.tracks.len()),
                Style::default().fg(theme.fg_dim),
            )));
            let total_ms: i64 = self
                .tracks
                .iter()
                .map(|t| t.duration_ms.unwrap_or(0) as i64)
                .sum();
            lines.push(Line::from(Span::styled(
                format_duration_ms(total_ms),
                Style::default().fg(theme.fg_dim),
            )));
        }

        let p = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(p, area);
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        covers: &mut CoverRegistry,
        show_cover: bool,
    ) {
        // Cover path, if any. `show_cover=false` (from the user's UI config)
        // drops the cover regardless of whether a file exists on disk —
        // useful for terminals where halfblock output renders as a blank gap.
        let cover_path: Option<PathBuf> = if show_cover {
            self.album
                .as_ref()
                .and_then(|a| a.cover_art_path.as_deref())
                .map(PathBuf::from)
                .filter(|p| p.exists())
        } else {
            None
        };

        // Top-level split: content + one-line footer (notice or track path).
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(1)])
            .split(area);

        // Responsive: show the info panel (cover + album text) beside the
        // track table only when there's enough horizontal room. The panel
        // is ~30 cols wide and the track table needs ~50 to show all its
        // columns without squeezing titles, so 80 is a reasonable floor.
        // On narrower terminals we collapse to just the tracks.
        const INFO_PANEL_WIDTH: u16 = 30;
        const INFO_PANEL_MIN_TOTAL: u16 = 80;
        let show_info_panel = chunks[0].width >= INFO_PANEL_MIN_TOTAL;

        let track_area_raw = if show_info_panel {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(INFO_PANEL_WIDTH), Constraint::Min(20)])
                .split(chunks[0]);
            self.render_info_panel(
                frame,
                cols[0],
                theme,
                covers,
                show_cover,
                cover_path.as_deref(),
            );
            cols[1]
        } else {
            chunks[0]
        };

        // 1-row top gutter on the track table — matches the info panel's
        // top padding so the header row doesn't sit flush against the
        // top of the content area.
        let track_area = Rect {
            y: track_area_raw.y.saturating_add(1),
            height: track_area_raw.height.saturating_sub(1),
            ..track_area_raw
        };

        // Track table — iterate over filtered indices
        let visible = self.filtered_indices();
        let total = visible.len();
        let visible_height = track_area.height.saturating_sub(1) as usize;
        self.scroll_offset = crate::tui::views::library::compute_scroll_offset(
            self.selected,
            self.scroll_offset,
            visible_height,
        );
        let scroll = self.scroll_offset;

        let mut rows = Vec::new();
        for (pos, &i) in visible
            .iter()
            .enumerate()
            .take(total.min(scroll + visible_height))
            .skip(scroll)
        {
            let track = &self.tracks[i];
            let is_selected = pos == self.selected;

            let num = track
                .track_number
                .map(track_table::numeric_cell)
                .unwrap_or_else(track_table::blank_numeric_cell);
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

            let gutter_span = if self.selection.contains(track.id) {
                Span::styled("▎", Style::default().fg(theme.accent))
            } else {
                Span::raw(" ")
            };
            let row = Row::new(vec![
                Cell::from(gutter_span),
                num,
                Cell::from(track.title.clone()),
                Cell::from(duration),
                Cell::from(Span::styled(track.tag_status.clone(), status_style)),
                Cell::from(bitrate),
            ]);

            rows.push(row.style(track_table::row_style(theme, pos, is_selected)));
        }

        let header = Row::new(vec![
            Cell::from(" "),
            track_table::numeric_header_cell("#", theme),
            track_table::header_cell("Title", theme),
            track_table::header_cell("Duration", theme),
            track_table::header_cell("Status", theme),
            track_table::header_cell("Bitrate", theme),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(rows, track_table::album_detail_widths()).header(header);

        frame.render_widget(table, track_area);

        // Single-line footer: notice takes priority over the selected
        // track's file path. Album-level metadata (formats, label, genre,
        // MB id) now lives in the info panel, so the bottom bar stays
        // focused on ephemeral state.
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
            frame.render_widget(p, chunks[1]);
        } else {
            let selected_track = visible.get(self.selected).and_then(|&i| self.tracks.get(i));
            if let Some(track) = selected_track {
                crate::tui::widgets::path_footer::render(frame, chunks[1], theme, &track.file_path);
            } else {
                let p = Paragraph::new("").style(Style::default().bg(theme.bg_alt));
                frame.render_widget(p, chunks[1]);
            }
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

        // Delete-confirm popup
        if let Some((_, popup)) = &self.pending_delete {
            popup.render(frame, area, theme);
        }

        // Overwrite-cover confirm popup
        if let Some(popup) = &self.pending_cover_overwrite {
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
                (OrganizeView::Summary, "Organize Preview", "Esc = close", 80)
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

/// `── Label ──` style heading for info-panel sections. Horizontal rules
/// aren't strictly necessary, but they break up the short text runs and
/// give the panel enough structure to read at a glance.
fn section_heading(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {} ──", label),
        Style::default().fg(theme.fg_muted),
    ))
}

/// `Key: value` line for the Tags section. Key stays muted, value picks
/// up the primary foreground so the eye lands there.
fn tag_line(key: &str, value: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{}: ", key), Style::default().fg(theme.fg_muted)),
        Span::styled(value.to_string(), Style::default().fg(theme.fg_dim)),
    ])
}

fn build_track_confirm(plan: &pruner::DeletePlan) -> ConfirmDelete {
    let n = plan.track_ids.len();
    let primary = if n == 1 {
        "Delete 1 track?".to_string()
    } else {
        format!("Delete {} tracks?", n)
    };
    let summary = format!("{} file(s) on disk", plan.deletable_file_count());
    let mut popup = ConfirmDelete::new("Confirm delete", primary).with_summary(summary);
    for line in &plan.album_summary_lines {
        popup = popup.with_detail(line.clone());
    }
    if plan.additional_albums > 0 {
        popup = popup.with_detail(format!("…and {} more album(s)", plan.additional_albums));
    }
    if !plan.collection_copies_to_delete.is_empty() {
        popup = popup.with_warning(format!(
            "{} collection copy file(s) will also be removed",
            plan.collection_copies_to_delete.len()
        ));
    }
    if !plan.files_outside_managed.is_empty() {
        popup = popup.with_detail(format!(
            "{} file(s) outside the music directory will be left on disk",
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

/// Open a directory in the system file manager.
/// Returns true if a file manager was successfully contacted.
///
/// On Linux, uses the D-Bus `org.freedesktop.FileManager1` interface first
/// (works even from terminal multiplexers like zellij/tmux where env vars
/// like DISPLAY/WAYLAND_DISPLAY aren't propagated). Falls back to spawning
/// common file managers directly.
/// On macOS, uses `open`. On Windows, uses `explorer.exe` (spawn-only:
/// explorer's exit code is unreliable, so success means "launched").
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

    #[cfg(target_os = "windows")]
    {
        return std::process::Command::new("explorer.exe")
            .arg(path)
            .stdout(null())
            .stderr(null())
            .stdin(null())
            .spawn()
            .is_ok();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let has_dbus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some();
        let has_display =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();

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
            for cmd in &[
                "nautilus", "dolphin", "thunar", "nemo", "pcmanfm", "xdg-open",
            ] {
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
