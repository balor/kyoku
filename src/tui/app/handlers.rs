//! Event/action handlers for the top-level `App`. Each view has its own
//! `handle_*_key` entry point; this module also owns navigation
//! (`switch_view`), search-bar refresh (`on_search_changed`), and data
//! reloads (`refresh`, `refresh_counts`).

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, AppAction, AppView};
use crate::db::queries;
use crate::tui::keybindings as keys;
use crate::tui::views::global_search::GlobalSearchAction;
use crate::tui::views::import::ConfirmCancel;
use crate::tui::widgets::confirm_delete::{ConfirmAction, ConfirmDelete};

impl App {
    pub fn switch_view(&mut self, view: AppView) {
        let music_dir = self.settings.library.music_dir.clone();
        match &view {
            AppView::Library => {
                self.library.load(&self.conn, &music_dir, None).ok();
            }
            AppView::Collections => {
                self.collections.load(&self.conn, None).ok();
            }
            AppView::AlbumDetail { album_id } => {
                self.album_detail
                    .load(&self.conn, &music_dir, *album_id)
                    .ok();
            }
            AppView::LooseTracks => {
                self.album_detail.load_loose(&self.conn, &music_dir).ok();
            }
            AppView::CollectionDetail { collection_id } => {
                self.collection_detail
                    .load(&self.conn, *collection_id, &music_dir)
                    .ok();
            }
            AppView::Import => {
                self.import.start(
                    &self.settings.library.inbox_dirs,
                    &self.settings.library.music_dir,
                    &self.conn,
                    self.settings.musicbrainz.rate_limit_ms,
                    self.settings.musicbrainz.name_script,
                    self.settings.tagging.write_tags,
                    self.settings.import.auto_match_threshold,
                    self.settings.import.match_candidates,
                    self.settings.database_file(),
                );
            }
            AppView::Editor { track_id } => {
                self.editor
                    .load(&self.conn, *track_id, &self.settings)
                    .ok();
            }
        }
        self.search.clear();
        self.search.focused = false;
        self.view = view;
    }

    pub(super) fn on_search_changed(&mut self) {
        let query = if self.search.value.is_empty() {
            None
        } else {
            Some(self.search.value.as_str())
        };
        let music_dir = self.settings.library.music_dir.clone();
        match self.view {
            AppView::Library => {
                self.library.load(&self.conn, &music_dir, query).ok();
            }
            AppView::Collections => {
                self.collections.load(&self.conn, query).ok();
            }
            AppView::AlbumDetail { .. } | AppView::LooseTracks => {
                self.album_detail
                    .set_filter(self.search.value.clone());
            }
            AppView::CollectionDetail { .. } => {
                self.collection_detail
                    .set_filter(self.search.value.clone());
            }
            _ => {}
        }
    }

    pub(super) fn handle_library_key(&mut self, key: KeyEvent) -> AppAction {
        // If the library has an open popup (e.g. add-to-collection), route to it first
        if !self.library.has_popup() {
            if keys::is_tab_switch(&key) || key.code == KeyCode::Char('c') {
                self.switch_view(AppView::Collections);
                return AppAction::None;
            }
            if key.code == KeyCode::Char('i') {
                self.switch_view(AppView::Import);
                return AppAction::None;
            }
        }
        let action = self.library.handle_key(key, &self.conn, &self.settings);
        self.process_library_action(action)
    }

    fn process_library_action(
        &mut self,
        action: crate::tui::views::library::LibraryAction,
    ) -> AppAction {
        match action {
            crate::tui::views::library::LibraryAction::None => AppAction::None,
            crate::tui::views::library::LibraryAction::OpenAlbum(id) => {
                self.switch_view(AppView::AlbumDetail { album_id: id });
                AppAction::None
            }
            crate::tui::views::library::LibraryAction::OpenLoose => {
                self.switch_view(AppView::LooseTracks);
                AppAction::None
            }
            crate::tui::views::library::LibraryAction::SortChanged => {
                let query = if self.search.value.is_empty() {
                    None
                } else {
                    Some(self.search.value.as_str())
                };
                self.library
                    .load(&self.conn, &self.settings.library.music_dir, query)
                    .ok();
                AppAction::None
            }
            crate::tui::views::library::LibraryAction::Deleted => {
                self.library
                    .load(&self.conn, &self.settings.library.music_dir, None)
                    .ok();
                self.refresh_counts();
                AppAction::None
            }
            crate::tui::views::library::LibraryAction::OrganizeAll => {
                if self.library.organize_plan.is_some() {
                    // Plan is already showing — Enter was pressed, so apply it
                    if let Some(plan) = self.library.organize_plan.take() {
                        match crate::core::organizer::apply_organize(
                            &self.conn,
                            &self.settings.library.music_dir,
                            &plan,
                            &self.settings.import.organize_operation,
                            &crate::core::organizer::cleanup_roots(&self.settings),
                        ) {
                            Ok(result) => {
                                let mut parts = Vec::new();
                                // Covers are just files — count them with the regular moves.
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
                                if result.orphans_cleaned > 0 {
                                    parts.push(format!(
                                        "{} orphans pruned",
                                        result.orphans_cleaned
                                    ));
                                }
                                if result.file_orphans_removed > 0 {
                                    parts.push(format!(
                                        "{} orphan files deleted",
                                        result.file_orphans_removed
                                    ));
                                }
                                if !result.errors.is_empty() {
                                    parts.push(format!("{} errors", result.errors.len()));
                                }
                                if result.prune_blocked_reason.is_some() {
                                    parts.push(
                                        "missing-source prune blocked (volume unavailable?)"
                                            .to_string(),
                                    );
                                }
                                if !parts.is_empty() {
                                    self.library.notice =
                                        Some(format!("Organized: {}", parts.join(", ")));
                                }
                            }
                            Err(e) => {
                                self.library.notice =
                                    Some(format!("Organize failed: {}", e));
                            }
                        }
                        self.library
                            .load(&self.conn, &self.settings.library.music_dir, None)
                            .ok();
                        self.refresh_counts();
                    }
                } else {
                    // Compute the plan and show it
                    match crate::core::organizer::plan_organize(
                        &self.conn,
                        &self.settings,
                        crate::core::organizer::OrganizeFilter::All,
                    ) {
                        Ok(plan) => {
                            self.library.organize_plan = Some(plan);
                            self.library.organize_details = false;
                            self.library.organize_scroll = 0;
                        }
                        Err(e) => {
                            self.library.notice =
                                Some(format!("Organize plan failed: {}", e));
                        }
                    }
                }
                AppAction::None
            }
        }
    }

    pub(super) fn handle_collections_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_tab_switch(&key) {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        let action = self.collections.handle_key(key, &self.conn, &self.settings);
        match action {
            crate::tui::views::collections::CollectionsAction::None => AppAction::None,
            crate::tui::views::collections::CollectionsAction::OpenCollection(id) => {
                self.switch_view(AppView::CollectionDetail { collection_id: id });
                AppAction::None
            }
            crate::tui::views::collections::CollectionsAction::Refresh => {
                self.collections.load(&self.conn, None).ok();
                AppAction::None
            }
            crate::tui::views::collections::CollectionsAction::OrganizeAll => {
                if self.collections.organize_plan.is_some() {
                    // Plan showing — Enter applies
                    if let Some(plan) = self.collections.organize_plan.take() {
                        if let Ok(_result) = crate::core::organizer::apply_organize(
                            &self.conn,
                            &self.settings.library.music_dir,
                            &plan,
                            &self.settings.import.organize_operation,
                            &crate::core::organizer::cleanup_roots(&self.settings),
                        ) {}
                        self.collections.load(&self.conn, None).ok();
                        self.refresh_counts();
                    }
                } else {
                    // Compute and show
                    if let Ok(plan) = crate::core::organizer::plan_organize(
                        &self.conn,
                        &self.settings,
                        crate::core::organizer::OrganizeFilter::All,
                    ) {
                        self.collections.organize_plan = Some(plan);
                    }
                }
                AppAction::None
            }
        }
    }

    pub(super) fn handle_detail_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.album_detail.has_popup() {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        // Tab jumps to Collections, matching library-view behaviour. Skip when
        // a popup owns input so Tab stays available inside those widgets.
        if !self.album_detail.has_popup() && keys::is_tab_switch(&key) {
            self.switch_view(AppView::Collections);
            return AppAction::None;
        }
        let action = self.album_detail.handle_key(key, &self.conn, &self.settings);
        match action {
            crate::tui::views::detail::DetailAction::None => AppAction::None,
            crate::tui::views::detail::DetailAction::EditTrack(id) => {
                self.editor_return_to = Some(self.view.clone());
                self.switch_view(AppView::Editor { track_id: id });
                AppAction::None
            }
            crate::tui::views::detail::DetailAction::Deleted => {
                // Reload view data; if the album itself is gone (all tracks
                // removed with delete_files or the last track), drop to library.
                match self.view.clone() {
                    AppView::AlbumDetail { album_id } => {
                        if queries::get_album(
                            &self.conn,
                            &self.settings.library.music_dir,
                            album_id,
                        )
                        .map(|o| o.is_none())
                        .unwrap_or(true)
                        {
                            self.switch_view(AppView::Library);
                        } else {
                            let prev = self.album_detail.selected;
                            self.album_detail
                                .load(&self.conn, &self.settings.library.music_dir, album_id)
                                .ok();
                            if prev < self.album_detail.tracks.len() {
                                self.album_detail.selected = prev;
                            }
                        }
                    }
                    AppView::LooseTracks => {
                        let prev = self.album_detail.selected;
                        self.album_detail
                            .load_loose(&self.conn, &self.settings.library.music_dir)
                            .ok();
                        if prev < self.album_detail.tracks.len() {
                            self.album_detail.selected = prev;
                        }
                    }
                    _ => {}
                }
                self.refresh_counts();
                AppAction::None
            }
            crate::tui::views::detail::DetailAction::Organize => {
                // Compute organize plan for the current album (or loose tracks)
                let filter = if let Some(album) = &self.album_detail.album {
                    crate::core::organizer::OrganizeFilter::AlbumId(album.id)
                } else {
                    crate::core::organizer::OrganizeFilter::Loose
                };
                match crate::core::organizer::plan_organize(
                    &self.conn,
                    &self.settings,
                    filter,
                ) {
                    Ok(plan) => {
                        self.album_detail.set_organize_plan(plan);
                    }
                    Err(e) => {
                        self.album_detail.set_notice(
                            crate::tui::views::detail::NoticeKind::Warning,
                            format!("Organize plan failed: {}", e),
                        );
                    }
                }
                AppAction::None
            }
        }
    }

    pub(super) fn handle_collection_detail_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.collection_detail.has_popup() {
            self.switch_view(AppView::Collections);
            return AppAction::None;
        }
        // Tab jumps to Library, matching collections-view behaviour. Skip when
        // a popup owns input so Tab stays available inside those widgets.
        if !self.collection_detail.has_popup() && keys::is_tab_switch(&key) {
            self.switch_view(AppView::Library);
            return AppAction::None;
        }
        let action =
            self.collection_detail
                .handle_key(key, &self.conn, &self.settings);
        match action {
            crate::tui::views::collections::CollectionDetailAction::None => AppAction::None,
            crate::tui::views::collections::CollectionDetailAction::Deleted => {
                if let AppView::CollectionDetail { collection_id } = self.view {
                    self.collection_detail
                        .load(&self.conn, collection_id, &self.settings.library.music_dir)
                        .ok();
                }
                self.refresh_counts();
                AppAction::None
            }
            crate::tui::views::collections::CollectionDetailAction::EditTrack(id) => {
                self.editor_return_to = Some(self.view.clone());
                self.switch_view(AppView::Editor { track_id: id });
                AppAction::None
            }
            crate::tui::views::collections::CollectionDetailAction::OpenDir => {
                let visible = self.collection_detail.filtered_indices();
                let track = visible
                    .get(self.collection_detail.selected)
                    .and_then(|&i| self.collection_detail.tracks.get(i));
                if let Some(track) = track {
                    let file_path = self
                        .collection_detail
                        .collection_paths
                        .get(&track.id)
                        .cloned()
                        .unwrap_or_else(|| track.file_path.clone());
                    let path = std::path::Path::new(&file_path);
                    if let Some(parent) = path.parent() {
                        if !parent.exists() {
                            self.collection_detail.notice =
                                Some(format!("Directory not found: {}", parent.display()));
                        } else if !crate::tui::views::detail::open_directory(parent) {
                            self.collection_detail.notice = Some(format!(
                                "Could not open file manager — path: {}",
                                parent.display()
                            ));
                        }
                    }
                }
                AppAction::None
            }
            crate::tui::views::collections::CollectionDetailAction::Organize => {
                if let Some(coll) = &self.collection_detail.collection {
                    let coll_name = coll.name.clone();

                    if self.collection_detail.organize_plan.is_some() {
                        // Plan already showing — Enter was pressed, apply it
                        if let Some(plan) = self.collection_detail.organize_plan.take() {
                            if let Ok(result) = crate::core::organizer::apply_organize(
                                &self.conn,
                                &self.settings.library.music_dir,
                                &plan,
                                &self.settings.import.organize_operation,
                                &crate::core::organizer::cleanup_roots(&self.settings),
                            ) {
                                let mut parts = Vec::new();
                                let moved_total = result.moved + result.covers_moved;
                                if moved_total > 0 {
                                    parts.push(format!("{} moved", moved_total));
                                }
                                if result.copied > 0 {
                                    parts.push(format!("{} copied", result.copied));
                                }
                                if result.dirs_cleaned > 0 {
                                    parts.push(format!(
                                        "{} dirs cleaned",
                                        result.dirs_cleaned
                                    ));
                                }
                                if result.file_orphans_removed > 0 {
                                    parts.push(format!(
                                        "{} orphan files deleted",
                                        result.file_orphans_removed
                                    ));
                                }
                                if !result.errors.is_empty() {
                                    parts.push(format!(
                                        "{} errors",
                                        result.errors.len()
                                    ));
                                }
                                // Stash notice — we can't set it on the view
                                // directly since we'll reload below
                                if !parts.is_empty() {
                                    let _notice =
                                        format!("Organized: {}", parts.join(", "));
                                }
                            }
                            // Reload to reflect new paths
                            if let AppView::CollectionDetail { collection_id } = self.view {
                                self.collection_detail
                                    .load(
                                        &self.conn,
                                        collection_id,
                                        &self.settings.library.music_dir,
                                    )
                                    .ok();
                            }
                            self.refresh_counts();
                        }
                    } else {
                        // Compute and show the plan
                        if let Ok(plan) = crate::core::organizer::plan_organize(
                            &self.conn,
                            &self.settings,
                            crate::core::organizer::OrganizeFilter::Collection(coll_name),
                        ) {
                            self.collection_detail.organize_plan = Some(plan);
                        }
                    }
                }
                AppAction::None
            }
        }
    }

    pub(super) fn handle_import_key(&mut self, key: KeyEvent) -> AppAction {
        // On the Complete step: any keypress returns to the library.
        if self.import.is_complete() {
            self.switch_view(AppView::Library);
            self.refresh_counts();
            return AppAction::None;
        }

        // Cancel-confirmation popup captures input when visible.
        if let Some(state) = &mut self.import.confirm_cancel {
            match state.popup.handle_key(key) {
                ConfirmAction::Confirm { .. } => {
                    let quit = state.quit_on_confirm;
                    self.import.confirm_cancel = None;
                    self.switch_view(AppView::Library);
                    self.refresh_counts();
                    if quit {
                        return AppAction::Quit;
                    }
                    return AppAction::None;
                }
                ConfirmAction::Cancel => {
                    self.import.confirm_cancel = None;
                    return AppAction::None;
                }
                ConfirmAction::None => return AppAction::None,
            }
        }

        // When the wizard is capturing text input (e.g. custom-path field),
        // Esc is handled inside the view (clear input / exit custom mode)
        // — don't cancel the whole wizard unless the view itself decides.
        let capturing = self.import.is_capturing_input();
        // Snapshot the step so we can tell *which* captured widget just
        // closed in response to Esc. Only the SelectSource custom-path
        // case wants an Esc-on-empty to also cancel the wizard; popups
        // during Review (collection picker, MBID input) should only
        // close the popup.
        let step_before = self.import.step.clone();

        if !capturing && keys::is_back(&key) && self.import.can_cancel() {
            self.import.confirm_cancel = Some(ConfirmCancel {
                popup: ConfirmDelete::new(
                    "Cancel import",
                    "Leave the import wizard?",
                )
                .with_summary("Any in-progress selections will be lost.")
                .without_checkbox(),
                quit_on_confirm: false,
            });
            return AppAction::None;
        }
        if !capturing && keys::is_quit(&key) && self.import.can_cancel() {
            self.import.confirm_cancel = Some(ConfirmCancel {
                popup: ConfirmDelete::new(
                    "Quit kyoku",
                    "Leave the import wizard and quit?",
                )
                .with_summary("Any in-progress selections will be lost.")
                .without_checkbox(),
                quit_on_confirm: true,
            });
            return AppAction::None;
        }
        self.import.handle_key(key, &self.conn);

        // If the view cleared its capturing state in response to Esc on an
        // empty custom-path input, treat that as a wizard cancel. Only
        // applies to SelectSource — popups in Review handle their own Esc.
        if capturing
            && keys::is_back(&key)
            && !self.import.is_capturing_input()
            && step_before == crate::tui::views::import::ImportStep::SelectSource
        {
            self.switch_view(AppView::Library);
            self.refresh_counts();
            return AppAction::None;
        }
        AppAction::None
    }

    fn refresh_counts(&mut self) {
        self.track_count = queries::count_tracks(&self.conn).unwrap_or(0);
        self.inbox_count = crate::core::importer::scan_inbox(
            &self.conn,
            &self.settings.library.music_dir,
            &self.settings.library.inbox_dirs,
        )
        .map(|v| v.len())
        .unwrap_or(0);
    }

    /// Full refresh: reloads counts and the current view's data from disk/DB.
    /// Useful when files are added to the inbox while the TUI is running.
    pub(super) fn refresh(&mut self) {
        self.refresh_counts();

        let search_query = if self.search.value.is_empty() {
            None
        } else {
            Some(self.search.value.clone())
        };

        let music_dir = self.settings.library.music_dir.clone();
        match &self.view {
            AppView::Library => {
                self.library
                    .load(&self.conn, &music_dir, search_query.as_deref())
                    .ok();
            }
            AppView::Collections => {
                self.collections
                    .load(&self.conn, search_query.as_deref())
                    .ok();
            }
            AppView::AlbumDetail { album_id } => {
                let prev = self.album_detail.selected;
                self.album_detail
                    .load(&self.conn, &music_dir, *album_id)
                    .ok();
                if prev < self.album_detail.tracks.len() {
                    self.album_detail.selected = prev;
                }
            }
            AppView::LooseTracks => {
                let prev = self.album_detail.selected;
                self.album_detail.load_loose(&self.conn, &music_dir).ok();
                if prev < self.album_detail.tracks.len() {
                    self.album_detail.selected = prev;
                }
            }
            AppView::CollectionDetail { collection_id } => {
                let prev = self.collection_detail.selected;
                self.collection_detail
                    .load(&self.conn, *collection_id, &music_dir)
                    .ok();
                if prev < self.collection_detail.tracks.len() {
                    self.collection_detail.selected = prev;
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_global_search_action(&mut self, action: GlobalSearchAction) -> AppAction {
        match action {
            GlobalSearchAction::None => AppAction::None,
            GlobalSearchAction::Close => {
                self.global_search_open = false;
                AppAction::None
            }
            GlobalSearchAction::OpenAlbum(id) => {
                self.global_search_open = false;
                self.switch_view(AppView::AlbumDetail { album_id: id });
                AppAction::None
            }
            GlobalSearchAction::OpenCollection(id) => {
                self.global_search_open = false;
                self.switch_view(AppView::CollectionDetail { collection_id: id });
                AppAction::None
            }
            GlobalSearchAction::OpenTrackAlbum { track_id, album_id } => {
                self.global_search_open = false;
                self.switch_view(AppView::AlbumDetail { album_id });
                // Try to move cursor to that track
                if let Some(pos) = self
                    .album_detail
                    .tracks
                    .iter()
                    .position(|t| t.id == track_id)
                {
                    self.album_detail.selected = pos;
                }
                AppAction::None
            }
        }
    }

    pub(super) fn handle_editor_key(&mut self, key: KeyEvent) -> AppAction {
        if keys::is_back(&key) && !self.editor.is_editing() {
            // Return to the view we came from without reloading
            // (preserves cursor position in album/collection detail)
            let return_view = self.editor_return_to.take().unwrap_or(AppView::Library);

            // If the editor made changes, refresh the underlying view's data
            // but keep the cursor in place by re-running load (which clamps selection).
            let music_dir = self.settings.library.music_dir.clone();
            match &return_view {
                AppView::AlbumDetail { album_id } => {
                    let prev_selected = self.album_detail.selected;
                    self.album_detail
                        .load(&self.conn, &music_dir, *album_id)
                        .ok();
                    if prev_selected < self.album_detail.tracks.len() {
                        self.album_detail.selected = prev_selected;
                    }
                }
                AppView::LooseTracks => {
                    let prev_selected = self.album_detail.selected;
                    self.album_detail.load_loose(&self.conn, &music_dir).ok();
                    if prev_selected < self.album_detail.tracks.len() {
                        self.album_detail.selected = prev_selected;
                    }
                }
                AppView::CollectionDetail { collection_id } => {
                    let prev_selected = self.collection_detail.selected;
                    self.collection_detail
                        .load(&self.conn, *collection_id, &music_dir)
                        .ok();
                    if prev_selected < self.collection_detail.tracks.len() {
                        self.collection_detail.selected = prev_selected;
                    }
                }
                _ => {}
            }

            self.view = return_view;
            return AppAction::None;
        }
        self.editor.handle_key(key, &self.conn);
        AppAction::None
    }
}
