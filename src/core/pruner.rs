//! Batch deletion of tracks and albums (rows + optional on-disk files).
//!
//! Kept separate from `organizer` because the two have different jobs: the
//! organizer *moves* files into their canonical locations, the pruner *removes*
//! DB rows and optionally the files they point at. The pruner still leans on
//! two safety helpers the organizer exposes (`cleanup_roots`,
//! `remove_empty_parents`) so the managed-roots invariant is shared between
//! both code paths.
//!
//! File-path safety: `DeletePlan::files_to_delete` only contains paths that
//! sit strictly inside one of the `managed_roots` passed at planning time
//! (music_dir + inbox dirs). Anything else lands in `files_outside_managed`
//! and is never touched — regardless of the user's "delete files" choice.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::core::organizer::remove_empty_parents;
use crate::db::queries;
use crate::error::Result;

/// Generalised batch-delete plan for tracks and albums. Built by
/// [`plan_delete_tracks`] / [`plan_delete_albums`] and applied by
/// [`apply_delete_plan`].
#[derive(Debug, Default, Clone)]
pub struct DeletePlan {
    /// Track DB rows that will be removed. Includes everything in
    /// `album_ids`' track lists when planning an album delete.
    pub track_ids: Vec<i64>,
    /// Album DB rows that will be removed after the tracks go.
    pub album_ids: Vec<i64>,
    /// Primary files (tracks.file_path) eligible for deletion.
    pub files_to_delete: Vec<PathBuf>,
    /// Collection-copy paths (collection_tracks.collection_file_path) that
    /// will be deleted alongside their primary files.
    pub collection_copies_to_delete: Vec<PathBuf>,
    /// Paths skipped because they sit outside every managed root.
    pub files_outside_managed: Vec<PathBuf>,
    /// Pre-rendered "Artist / Album (N tracks)" lines for the confirm popup.
    /// Up to 3 entries; surplus albums are summarised by `additional_albums`.
    pub album_summary_lines: Vec<String>,
    /// Number of affected albums beyond those shown in `album_summary_lines`.
    pub additional_albums: usize,
}

#[derive(Debug, Default)]
pub struct DeleteReport {
    pub files_deleted: u32,
    pub tracks_deleted: u32,
    pub albums_deleted: u32,
    pub dirs_cleaned: u32,
    pub errors: Vec<(String, String)>,
}

impl DeletePlan {
    pub fn is_empty(&self) -> bool {
        self.track_ids.is_empty() && self.album_ids.is_empty()
    }

    /// Total file count the user can opt into deleting (primaries + copies).
    pub fn deletable_file_count(&self) -> usize {
        self.files_to_delete.len() + self.collection_copies_to_delete.len()
    }
}

/// Returns `true` iff `path` is strictly inside at least one managed root.
fn path_is_managed(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|r| path.starts_with(r) && path != r.as_path())
}

/// Classify a file path into the plan — either queued for deletion or tagged
/// as outside-managed-roots (never touched).
fn classify_file(
    plan: &mut DeletePlan,
    path: PathBuf,
    managed_roots: &[PathBuf],
    is_collection_copy: bool,
) {
    if path_is_managed(&path, managed_roots) {
        if is_collection_copy {
            plan.collection_copies_to_delete.push(path);
        } else {
            plan.files_to_delete.push(path);
        }
    } else {
        plan.files_outside_managed.push(path);
    }
}

/// Plan deletion of a set of tracks. `managed_roots` is the whitelist of
/// directories under which files may be deleted (typically
/// [`crate::core::organizer::cleanup_roots`] output). Files outside go into
/// `files_outside_managed` and are reported to the user but never removed.
pub fn plan_delete_tracks(
    conn: &Connection,
    track_ids: &[i64],
    managed_roots: &[PathBuf],
) -> Result<DeletePlan> {
    let mut plan = DeletePlan::default();
    if track_ids.is_empty() {
        return Ok(plan);
    }
    let infos = queries::get_tracks_delete_info(conn, track_ids)?;

    // Collect unique album ids for summary purposes (NOT for deletion —
    // plan_delete_tracks never removes album rows).
    use std::collections::BTreeMap;
    let mut album_counts: BTreeMap<i64, u32> = BTreeMap::new();

    for info in &infos {
        plan.track_ids.push(info.track_id);
        if !info.file_path.is_empty() {
            classify_file(&mut plan, PathBuf::from(&info.file_path), managed_roots, false);
        }
        for copy in &info.collection_copies {
            classify_file(&mut plan, PathBuf::from(copy), managed_roots, true);
        }
        if let Some(aid) = info.album_id {
            *album_counts.entry(aid).or_insert(0) += 1;
        }
    }

    fill_album_summary(conn, &mut plan, album_counts);
    Ok(plan)
}

/// Plan deletion of a set of albums. Every track under the album is added to
/// `track_ids`; the album rows go in `album_ids` and are deleted after their
/// tracks during apply.
pub fn plan_delete_albums(
    conn: &Connection,
    album_ids: &[i64],
    managed_roots: &[PathBuf],
) -> Result<DeletePlan> {
    let mut plan = DeletePlan::default();
    if album_ids.is_empty() {
        return Ok(plan);
    }

    let track_ids = queries::list_tracks_for_albums(conn, album_ids)?;
    let infos = queries::get_tracks_delete_info(conn, &track_ids)?;

    use std::collections::BTreeMap;
    let mut album_counts: BTreeMap<i64, u32> = BTreeMap::new();

    for info in &infos {
        plan.track_ids.push(info.track_id);
        if !info.file_path.is_empty() {
            classify_file(&mut plan, PathBuf::from(&info.file_path), managed_roots, false);
        }
        for copy in &info.collection_copies {
            classify_file(&mut plan, PathBuf::from(copy), managed_roots, true);
        }
        if let Some(aid) = info.album_id {
            *album_counts.entry(aid).or_insert(0) += 1;
        }
    }

    // Even empty albums (no tracks) should appear in the summary + be deleted.
    for aid in album_ids {
        album_counts.entry(*aid).or_insert(0);
    }

    plan.album_ids = album_ids.to_vec();
    plan.album_ids.sort_unstable();
    plan.album_ids.dedup();

    fill_album_summary(conn, &mut plan, album_counts);
    Ok(plan)
}

fn fill_album_summary(
    conn: &Connection,
    plan: &mut DeletePlan,
    album_counts: std::collections::BTreeMap<i64, u32>,
) {
    const MAX_SHOWN: usize = 3;
    let mut iter = album_counts.iter();
    for (album_id, n) in iter.by_ref().take(MAX_SHOWN) {
        let label = queries::get_album_label(conn, *album_id)
            .ok()
            .flatten()
            .map(|(artist, title)| format!("{} / {} ({})", artist, title, n))
            .unwrap_or_else(|| format!("(album #{}) ({})", album_id, n));
        plan.album_summary_lines.push(label);
    }
    plan.additional_albums = iter.count();
}

/// Execute a `DeletePlan`. `cleanup_roots` mirrors the managed-roots list
/// used at planning time; only directories strictly inside one of them will
/// be pruned after file removal.
///
/// When `delete_files` is `false`:
///   - track rows are still removed from the DB (matches collection-delete
///     semantics),
///   - physical files stay on disk untouched.
pub fn apply_delete_plan(
    conn: &Connection,
    plan: &DeletePlan,
    delete_files: bool,
    cleanup_roots: &[PathBuf],
) -> Result<DeleteReport> {
    let mut report = DeleteReport::default();
    let mut emptied_dirs: Vec<PathBuf> = Vec::new();

    if delete_files {
        for p in plan
            .files_to_delete
            .iter()
            .chain(plan.collection_copies_to_delete.iter())
        {
            if !p.exists() {
                continue;
            }
            // Defensive second check — even if a caller passes a plan built
            // against different roots, refuse to touch unmanaged paths.
            if !path_is_managed(p, cleanup_roots) {
                continue;
            }
            match std::fs::remove_file(p) {
                Ok(()) => {
                    report.files_deleted += 1;
                    if let Some(parent) = p.parent() {
                        emptied_dirs.push(parent.to_path_buf());
                    }
                }
                Err(e) => report.errors.push((p.display().to_string(), e.to_string())),
            }
        }
    }

    // Delete track rows (cascades collection_tracks via FK).
    for &tid in &plan.track_ids {
        if queries::delete_track(conn, tid).is_ok() {
            report.tracks_deleted += 1;
        }
    }

    // Delete album rows after their tracks. With FK=ON, this would error if
    // tracks were still pointing at the album — but we just deleted them.
    for &aid in &plan.album_ids {
        if queries::delete_album(conn, aid).is_ok() {
            report.albums_deleted += 1;
        }
    }

    if delete_files {
        emptied_dirs.sort();
        emptied_dirs.dedup();
        emptied_dirs.reverse();
        for dir in &emptied_dirs {
            report.dirs_cleaned += remove_empty_parents(dir, cleanup_roots);
        }
    }

    Ok(report)
}

#[cfg(test)]
#[path = "pruner_tests.rs"]
mod tests;
