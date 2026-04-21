use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::Settings;
use crate::core::template::{self, TemplateVars};
use crate::db::queries;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct FileMove {
    pub track_id: i64,
    pub from: PathBuf,
    pub to: PathBuf,
    /// If set, this move's destination is also the primary file location
    /// for the given collection — `apply_organize` will update
    /// `collection_tracks.collection_file_path` accordingly.
    pub also_collection: Option<(i64, String)>,
}

#[derive(Debug, Clone)]
pub struct FileCopy {
    pub track_id: i64,
    pub collection_id: i64,
    pub collection_name: String,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Default)]
pub struct OrganizePlan {
    pub moves: Vec<FileMove>,
    pub copies: Vec<FileCopy>,
    pub skipped: usize,
    /// Tracks whose source file no longer exists on disk. These rows are
    /// orphaned — they point at paths that have been moved/deleted/renamed
    /// outside of kyoku. `apply_organize` will delete these DB rows.
    pub missing_sources: Vec<(i64, PathBuf, String)>,
}

#[derive(Debug, Default)]
pub struct OrganizeResult {
    pub moved: u32,
    pub copied: u32,
    pub errors: Vec<(String, String)>,
    pub dirs_cleaned: u32,
    pub orphans_cleaned: u32,
}

#[derive(Debug, Clone)]
pub enum OrganizeFilter {
    All,
    Artist(String),
    Album(String),
    AlbumId(i64),
    Loose,
    Path(PathBuf),
    Collection(String),
}

/// Compute an organize plan without any side effects.
pub fn plan_organize(
    conn: &Connection,
    settings: &Settings,
    filter: OrganizeFilter,
) -> Result<OrganizePlan> {
    let music_dir = &settings.library.music_dir;
    let mut plan = OrganizePlan::default();

    let tracks = queries::get_all_tracks_for_organize(conn, &filter)?;

    // Collision tracking: `tracks.file_path` has a UNIQUE constraint in the
    // DB, so two tracks cannot end up with the same destination path.
    // We start with every existing track path, remove the ones being moved
    // (those slots will be freed), and disambiguate proposed targets
    // against this set before committing them to the plan.
    use std::collections::HashSet;
    let mut used_paths: HashSet<PathBuf> = queries::list_all_track_paths(conn)?
        .into_iter()
        .map(|(_, p)| PathBuf::from(p))
        .collect();
    for t in &tracks {
        used_paths.remove(&PathBuf::from(&t.file_path));
    }

    // Helper: return a variant of `target` that isn't already in `used`,
    // appending " (2)", " (3)", … before the extension. Inserts the final
    // chosen path into `used` as a side effect.
    let disambiguate = |target: PathBuf, used: &mut HashSet<PathBuf>| -> PathBuf {
        if !used.contains(&target) {
            used.insert(target.clone());
            return target;
        }
        let parent = target.parent().map(|p| p.to_path_buf());
        let stem = target
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let ext = target
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        for i in 2..1000 {
            let new_name = if ext.is_empty() {
                format!("{} ({})", stem, i)
            } else {
                format!("{} ({}).{}", stem, i, ext)
            };
            let candidate = match &parent {
                Some(p) => p.join(&new_name),
                None => PathBuf::from(&new_name),
            };
            if !used.contains(&candidate) {
                used.insert(candidate.clone());
                return candidate;
            }
        }
        // Fallback: just return the original and let the DB complain
        used.insert(target.clone());
        target
    };

    for t in &tracks {
        // Detect orphaned DB rows: tracks whose source file no longer exists.
        // These happen when a previous organize run partially succeeded and
        // left the DB pointing at gone files, or when the user deleted files
        // manually. We collect them and clean up the rows during apply.
        let from_check = PathBuf::from(&t.file_path);
        if !from_check.exists() {
            plan.missing_sources
                .push((t.id, from_check, t.title.clone()));
            continue;
        }

        let vars = TemplateVars {
            artist: t.artist.clone().unwrap_or_default(),
            album_artist: t.album_artist.clone().unwrap_or_default(),
            album: t.album_title.clone().unwrap_or_default(),
            year: t.year.map(|y| y.to_string()).unwrap_or_default(),
            title: t.title.clone(),
            track: t.track_number.unwrap_or(0),
            disc: t.disc_number,
            genre: t.genre.clone().unwrap_or_default(),
            ext: Path::new(&t.file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp3")
                .to_lowercase(),
            label: t.label.clone().unwrap_or_default(),
            collection: String::new(),
        };

        let has_album = t.album_title.is_some();
        let from = PathBuf::from(&t.file_path);

        // Metadata-starved tracks (no track number, no artist, no album_artist)
        // would render as garbage like "00 Unknown - <stem>.ext" — keep the
        // original filename instead while still honouring the template's directory.
        let metadata_starved = t.track_number.unwrap_or(0) == 0
            && t.artist.as_deref().unwrap_or("").trim().is_empty()
            && t.album_artist.as_deref().unwrap_or("").trim().is_empty();
        let preserve_filename = |target: PathBuf| -> PathBuf {
            if !metadata_starved {
                return target;
            }
            match (target.parent(), from.file_name()) {
                (Some(dir), Some(name)) => dir.join(name),
                _ => target,
            }
        };

        // Sort collections deterministically by ID (oldest first)
        let mut collections = t.collections.clone();
        collections.sort_by_key(|(id, _, _)| *id);

        // Helper to compute a collection's raw target path (pre-disambiguation)
        let collection_target = |coll_name: &str, coll_template: &Option<String>| -> PathBuf {
            let tmpl = coll_template
                .as_deref()
                .unwrap_or(&settings.library.collection_path_template);
            let mut coll_vars = vars.clone();
            coll_vars.collection = coll_name.to_string();
            preserve_filename(music_dir.join(template::render_path(tmpl, &coll_vars)))
        };

        if has_album {
            // Album track: move to album hierarchy + copy to each collection
            let tmpl = if t.disc_total.unwrap_or(1) <= 1 {
                &settings.library.path_template_single_disc
            } else {
                &settings.library.path_template
            };
            let raw_target = preserve_filename(music_dir.join(template::render_path(tmpl, &vars)));

            // Copies must read from the post-move location because apply_organize
            // runs moves before copies — by then `from` has been renamed.
            let copy_source: PathBuf = if from == raw_target {
                // Already in place — reserve the slot so nothing else takes it
                used_paths.insert(raw_target.clone());
                plan.skipped += 1;
                raw_target
            } else {
                let target = disambiguate(raw_target, &mut used_paths);
                if from == target {
                    // Disambiguation handed us back our own path (e.g. another
                    // track now owns the un-suffixed slot). Already in place.
                    plan.skipped += 1;
                    target
                } else {
                    plan.moves.push(FileMove {
                        track_id: t.id,
                        from: from.clone(),
                        to: target.clone(),
                        also_collection: None,
                    });
                    target
                }
            };

            // One copy per collection — skip if the target already exists on disk
            // (collection was already organized).
            for (coll_id, coll_name, coll_template) in &collections {
                let raw = collection_target(coll_name, coll_template);
                if raw.exists() {
                    used_paths.insert(raw);
                    plan.skipped += 1;
                } else {
                    let target = disambiguate(raw, &mut used_paths);
                    plan.copies.push(FileCopy {
                        track_id: t.id,
                        collection_id: *coll_id,
                        collection_name: coll_name.clone(),
                        from: copy_source.clone(),
                        to: target,
                    });
                }
            }
        } else if !collections.is_empty() {
            // Loose track in collections: MOVE to first collection's folder
            let (first_id, first_name, first_template) = &collections[0];
            let raw_primary = collection_target(first_name, first_template);

            // Same post-move-location rule as above.
            let copy_source: PathBuf = if from == raw_primary {
                used_paths.insert(raw_primary.clone());
                plan.skipped += 1;
                raw_primary
            } else {
                let primary_target = disambiguate(raw_primary, &mut used_paths);
                if from == primary_target {
                    plan.skipped += 1;
                    primary_target
                } else {
                    plan.moves.push(FileMove {
                        track_id: t.id,
                        from: from.clone(),
                        to: primary_target.clone(),
                        also_collection: Some((*first_id, first_name.clone())),
                    });
                    primary_target
                }
            };

            // COPY to each additional collection's folder — skip if already exists
            for (coll_id, coll_name, coll_template) in &collections[1..] {
                let raw = collection_target(coll_name, coll_template);
                if raw.exists() {
                    used_paths.insert(raw);
                    plan.skipped += 1;
                } else {
                    let target = disambiguate(raw, &mut used_paths);
                    plan.copies.push(FileCopy {
                        track_id: t.id,
                        collection_id: *coll_id,
                        collection_name: coll_name.clone(),
                        from: copy_source.clone(),
                        to: target,
                    });
                }
            }
        } else {
            // Loose track, no collections: move to _loose/ folder
            let tmpl = &settings.library.loose_path_template;
            let raw_target = preserve_filename(music_dir.join(template::render_path(tmpl, &vars)));

            if from == raw_target {
                used_paths.insert(raw_target);
                plan.skipped += 1;
            } else {
                let target = disambiguate(raw_target, &mut used_paths);
                if from == target {
                    plan.skipped += 1;
                } else {
                    plan.moves.push(FileMove {
                        track_id: t.id,
                        from,
                        to: target,
                        also_collection: None,
                    });
                }
            }
        }
    }

    Ok(plan)
}

/// Execute an organize plan: move/copy files, update DB paths, clean up.
///
/// Per-track atomicity: each track's filesystem op is paired with its DB path
/// update(s) as a best-effort unit. We rely on SQLite's auto-commit (no outer
/// transaction) so that each successful `update_*_path` is durable immediately.
/// Errors on any single track (fs failure OR DB failure after a successful
/// move) are recorded in `result.errors` and the loop continues with the next
/// track. Worst-case on abrupt termination: exactly one file moved on disk
/// whose row was not yet updated in the DB.
pub fn apply_organize(
    conn: &Connection,
    plan: &OrganizePlan,
    operation: &str,
    cleanup_roots: &[PathBuf],
) -> Result<OrganizeResult> {
    let mut result = OrganizeResult::default();
    let mut emptied_dirs: Vec<PathBuf> = Vec::new();

    for m in &plan.moves {
        if let Some(parent) = m.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let outcome = if operation == "copy" {
            std::fs::copy(&m.from, &m.to).map(|_| ())
        } else {
            std::fs::rename(&m.from, &m.to).or_else(|_| {
                // rename fails across filesystems — fallback to copy+delete
                std::fs::copy(&m.from, &m.to).map(|_| ())?;
                std::fs::remove_file(&m.from)
            })
        };

        match outcome {
            Ok(()) => {
                let new_path = m.to.display().to_string();

                // Update tracks.file_path. On failure, record a detailed error
                // (file is already on disk at `new_path`) and skip this track's
                // remaining updates — but keep processing the rest of the plan.
                if let Err(e) = queries::update_track_path(conn, m.track_id, &new_path) {
                    result.errors.push((
                        m.from.display().to_string(),
                        format!("file moved to {new_path} but DB update failed: {e}"),
                    ));
                    continue;
                }

                // If this move also represents a collection's primary file location,
                // update collection_tracks.collection_file_path so the DB knows.
                if let Some((coll_id, _)) = &m.also_collection
                    && let Err(e) = queries::update_collection_track_path(
                        conn,
                        *coll_id,
                        m.track_id,
                        &new_path,
                    ) {
                        result.errors.push((
                            m.from.display().to_string(),
                            format!(
                                "file moved and tracks row updated, but collection_tracks update failed: {e}"
                            ),
                        ));
                        continue;
                    }

                // Only count moves that are fully consistent (fs + every DB update).
                result.moved += 1;

                // Track the source directory for cleanup
                if operation != "copy"
                    && let Some(parent) = m.from.parent() {
                        emptied_dirs.push(parent.to_path_buf());
                    }
            }
            Err(e) => {
                result.errors.push((m.from.display().to_string(), e.to_string()));
            }
        }
    }

    for c in &plan.copies {
        if let Some(parent) = c.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        match std::fs::copy(&c.from, &c.to) {
            Ok(_) => {
                let new_path = c.to.display().to_string();
                if let Err(e) = queries::update_collection_track_path(
                    conn,
                    c.collection_id,
                    c.track_id,
                    &new_path,
                ) {
                    result.errors.push((
                        c.from.display().to_string(),
                        format!("file copied to {new_path} but DB update failed: {e}"),
                    ));
                    continue;
                }
                result.copied += 1;
            }
            Err(e) => {
                result.errors.push((c.from.display().to_string(), e.to_string()));
            }
        }
    }

    // Delete orphaned DB rows (tracks whose source file no longer exists)
    for (track_id, _, _) in &plan.missing_sources {
        if queries::delete_track(conn, *track_id).is_ok() {
            result.orphans_cleaned += 1;
        }
    }

    // Clean up empty source directories (deepest first)
    emptied_dirs.sort();
    emptied_dirs.dedup();
    emptied_dirs.reverse();
    for dir in &emptied_dirs {
        result.dirs_cleaned += remove_empty_parents(dir, cleanup_roots);
    }

    Ok(result)
}

// ── Collection deletion ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DeleteCollectionPlan {
    pub collection_id: i64,
    pub collection_name: String,
    pub files_to_delete: Vec<PathBuf>,
    pub files_outside_music_dir: Vec<PathBuf>,
    pub orphaned_track_ids: Vec<i64>,
    /// Tracks whose `tracks.file_path` will be deleted but who have other
    /// collection copies that can be promoted to be the new file_path.
    /// Stored as (track_id, candidate new path).
    pub promote_paths: Vec<(i64, String)>,
}

#[derive(Debug, Default)]
pub struct DeleteCollectionResult {
    pub files_deleted: u32,
    pub dirs_cleaned: u32,
    pub tracks_orphaned_removed: u32,
    pub errors: Vec<(String, String)>,
}

/// Plan a collection deletion. Classifies files (inside/outside music_dir)
/// and identifies tracks that would be orphaned (no other home).
pub fn plan_delete_collection(
    conn: &Connection,
    collection_id: i64,
    music_dir: &Path,
) -> Result<DeleteCollectionPlan> {
    let homes = queries::get_collection_tracks_with_other_homes(conn, collection_id)?;
    let collection_name: String = conn
        .query_row(
            "SELECT name FROM collections WHERE id = ?1",
            [collection_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| String::from("(unknown)"));

    let mut files_to_delete = Vec::new();
    let mut files_outside_music_dir = Vec::new();
    let mut orphaned_track_ids = Vec::new();
    let mut promote_paths: Vec<(i64, String)> = Vec::new();

    for h in &homes {
        // Classify the collection file path (if any)
        if let Some(coll_path) = &h.collection_file_path {
            let p = PathBuf::from(coll_path);
            if p.starts_with(music_dir) {
                files_to_delete.push(p);
            } else {
                files_outside_music_dir.push(p);
            }
        }

        // Determine if this track has any other home
        let has_other_home = h.has_album || h.other_collection_count > 0;
        if !has_other_home {
            orphaned_track_ids.push(h.track_id);
            continue;
        }

        // Track has another home — if its tracks.file_path matches the
        // about-to-be-deleted collection file, we need to promote another path.
        if let Some(coll_path) = &h.collection_file_path
            && &h.track_file_path == coll_path && !h.has_album {
                // No album to fall back on, but other collection(s) exist.
                // Find one of those other collection_file_path values.
                if let Ok(Some(new_path)) = find_other_collection_path(conn, h.track_id, collection_id) {
                    promote_paths.push((h.track_id, new_path));
                }
            }
            // If has_album, file_path should already point at the album file
            // (set by organize when the album move happens).
    }

    Ok(DeleteCollectionPlan {
        collection_id,
        collection_name,
        files_to_delete,
        files_outside_music_dir,
        orphaned_track_ids,
        promote_paths,
    })
}

fn find_other_collection_path(
    conn: &Connection,
    track_id: i64,
    exclude_collection_id: i64,
) -> Result<Option<String>> {
    let path: Option<String> = conn
        .query_row(
            "SELECT collection_file_path FROM collection_tracks
             WHERE track_id = ?1 AND collection_id != ?2
                AND collection_file_path IS NOT NULL
             LIMIT 1",
            rusqlite::params![track_id, exclude_collection_id],
            |row| row.get(0),
        )
        .ok();
    Ok(path)
}

/// Execute a collection deletion plan. If `delete_files` is true, also removes
/// the physical files inside `music_dir` and any orphaned track DB rows.
pub fn apply_delete_collection(
    conn: &Connection,
    plan: &DeleteCollectionPlan,
    delete_files: bool,
    music_dir: &Path,
) -> Result<DeleteCollectionResult> {
    let mut result = DeleteCollectionResult::default();
    let mut emptied_dirs: Vec<PathBuf> = Vec::new();

    if delete_files {
        // Delete physical files (only those inside music_dir)
        for p in &plan.files_to_delete {
            if !p.exists() {
                continue;
            }
            match std::fs::remove_file(p) {
                Ok(()) => {
                    result.files_deleted += 1;
                    if let Some(parent) = p.parent() {
                        emptied_dirs.push(parent.to_path_buf());
                    }
                }
                Err(e) => {
                    result.errors.push((p.display().to_string(), e.to_string()));
                }
            }
        }
    }

    // Promote alternate file_paths for tracks that survive (have another home).
    // Done before the cascade so we don't accidentally end up pointing at a deleted file.
    for (track_id, new_path) in &plan.promote_paths {
        queries::update_track_path(conn, *track_id, new_path)?;
    }

    // Delete orphaned tracks entirely if user opted in.
    if delete_files {
        for &track_id in &plan.orphaned_track_ids {
            queries::delete_track(conn, track_id)?;
            result.tracks_orphaned_removed += 1;
        }
    }

    // Finally, drop the collection (cascade removes collection_tracks rows)
    queries::delete_collection(conn, plan.collection_id)?;

    // Clean up empty parent dirs
    if delete_files {
        emptied_dirs.sort();
        emptied_dirs.dedup();
        emptied_dirs.reverse();
        let roots = [music_dir.to_path_buf()];
        for dir in &emptied_dirs {
            result.dirs_cleaned += remove_empty_parents(dir, &roots);
        }
    }

    Ok(result)
}

/// Managed directory roots for directory-cleanup safety: music_dir plus any
/// configured inbox dirs. `apply_organize` / `apply_delete_collection` use
/// these as a whitelist when sweeping empty parents — nothing outside this
/// set is ever touched.
pub fn cleanup_roots(settings: &Settings) -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(1 + settings.library.inbox_dirs.len());
    roots.push(settings.library.music_dir.clone());
    roots.extend(settings.library.inbox_dirs.iter().cloned());
    roots
}

/// Remove a directory and its empty parents, stopping at the first non-empty
/// one. `roots` is a whitelist of managed directory roots (e.g. music_dir,
/// inbox dirs): the function never deletes a path equal to any root, never
/// climbs above any root, and refuses to touch `dir` at all unless it lives
/// strictly inside at least one of the roots. This is a safety floor against
/// accidentally sweeping up the user's home directory or filesystem root.
pub(crate) fn remove_empty_parents(dir: &Path, roots: &[PathBuf]) -> u32 {
    // Must live strictly inside at least one root. Being equal to a root
    // is not enough — roots themselves are sacrosanct.
    let is_inside_a_root = |p: &Path| -> bool {
        roots.iter().any(|r| p.starts_with(r) && p != r.as_path())
    };
    if !is_inside_a_root(dir) {
        return 0;
    }

    let mut cleaned = 0u32;
    let mut current = dir.to_path_buf();
    loop {
        // Never touch a path that is not strictly inside a managed root.
        if !is_inside_a_root(&current) {
            break;
        }
        match std::fs::read_dir(&current) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    if std::fs::remove_dir(&current).is_ok() {
                        cleaned += 1;
                    } else {
                        break;
                    }
                } else {
                    break; // Not empty
                }
            }
            Err(_) => break,
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    cleaned
}

// ── Track / album deletion ───────────────────────────────────────────

/// Generalised batch-delete plan for tracks and albums. Built by
/// [`plan_delete_tracks`] / [`plan_delete_albums`] and applied by
/// [`apply_delete_plan`].
///
/// File-path safety: `files_to_delete` only contains paths that sit strictly
/// inside one of the `managed_roots` passed at planning time (music_dir + inbox
/// dirs). Anything else lands in `files_outside_managed` and is never touched
/// — regardless of the user's "delete files" choice.
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
/// [`cleanup_roots`] output). Files outside go into
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
#[path = "organizer_tests.rs"]
mod tests;
