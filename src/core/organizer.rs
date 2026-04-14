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
            music_dir.join(template::render_path(tmpl, &coll_vars))
        };

        if has_album {
            // Album track: move to album hierarchy + copy to each collection
            let tmpl = if t.disc_total.unwrap_or(1) <= 1 {
                &settings.library.path_template_single_disc
            } else {
                &settings.library.path_template
            };
            let raw_target = music_dir.join(template::render_path(tmpl, &vars));

            if from == raw_target {
                // Already in place — reserve the slot so nothing else takes it
                used_paths.insert(raw_target);
                plan.skipped += 1;
            } else {
                let target = disambiguate(raw_target, &mut used_paths);
                plan.moves.push(FileMove {
                    track_id: t.id,
                    from: from.clone(),
                    to: target,
                    also_collection: None,
                });
            }

            // One copy per collection (collection_file_path has no UNIQUE
            // constraint, so raw targets are fine here — but we still
            // disambiguate on disk to avoid overwriting files.)
            for (coll_id, coll_name, coll_template) in &collections {
                let raw = collection_target(coll_name, coll_template);
                let target = disambiguate(raw, &mut used_paths);
                plan.copies.push(FileCopy {
                    track_id: t.id,
                    collection_id: *coll_id,
                    collection_name: coll_name.clone(),
                    from: from.clone(),
                    to: target,
                });
            }
        } else if !collections.is_empty() {
            // Loose track in collections: MOVE to first collection's folder
            let (first_id, first_name, first_template) = &collections[0];
            let raw_primary = collection_target(first_name, first_template);

            if from == raw_primary {
                used_paths.insert(raw_primary);
                plan.skipped += 1;
            } else {
                let primary_target = disambiguate(raw_primary, &mut used_paths);
                plan.moves.push(FileMove {
                    track_id: t.id,
                    from: from.clone(),
                    to: primary_target,
                    also_collection: Some((*first_id, first_name.clone())),
                });
            }

            // COPY to each additional collection's folder
            for (coll_id, coll_name, coll_template) in &collections[1..] {
                let raw = collection_target(coll_name, coll_template);
                let target = disambiguate(raw, &mut used_paths);
                plan.copies.push(FileCopy {
                    track_id: t.id,
                    collection_id: *coll_id,
                    collection_name: coll_name.clone(),
                    from: from.clone(),
                    to: target,
                });
            }
        } else {
            // Loose track, no collections: move to _loose/ folder
            let tmpl = &settings.library.loose_path_template;
            let raw_target = music_dir.join(template::render_path(tmpl, &vars));

            if from == raw_target {
                used_paths.insert(raw_target);
                plan.skipped += 1;
            } else {
                let target = disambiguate(raw_target, &mut used_paths);
                plan.moves.push(FileMove {
                    track_id: t.id,
                    from,
                    to: target,
                    also_collection: None,
                });
            }
        }
    }

    Ok(plan)
}

/// Execute an organize plan: move/copy files, update DB paths, clean up.
pub fn apply_organize(
    conn: &Connection,
    plan: &OrganizePlan,
    operation: &str,
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
                queries::update_track_path(conn, m.track_id, &m.to.display().to_string())?;
                result.moved += 1;

                // If this move also represents a collection's primary file location,
                // update collection_tracks.collection_file_path so the DB knows.
                if let Some((coll_id, _)) = &m.also_collection {
                    queries::update_collection_track_path(
                        conn,
                        *coll_id,
                        m.track_id,
                        &m.to.display().to_string(),
                    )?;
                }

                // Track the source directory for cleanup
                if operation != "copy" {
                    if let Some(parent) = m.from.parent() {
                        emptied_dirs.push(parent.to_path_buf());
                    }
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
                queries::update_collection_track_path(
                    conn,
                    c.collection_id,
                    c.track_id,
                    &c.to.display().to_string(),
                )?;
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
        result.dirs_cleaned += remove_empty_parents(dir);
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
        if let Some(coll_path) = &h.collection_file_path {
            if &h.track_file_path == coll_path && !h.has_album {
                // No album to fall back on, but other collection(s) exist.
                // Find one of those other collection_file_path values.
                if let Ok(Some(new_path)) = find_other_collection_path(conn, h.track_id, collection_id) {
                    promote_paths.push((h.track_id, new_path));
                }
            }
            // If has_album, file_path should already point at the album file
            // (set by organize when the album move happens).
        }
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
        for dir in &emptied_dirs {
            result.dirs_cleaned += remove_empty_parents(dir);
        }
    }

    Ok(result)
}

/// Remove a directory and its empty parents, stopping at the first non-empty one.
pub(crate) fn remove_empty_parents(dir: &Path) -> u32 {
    let mut cleaned = 0u32;
    let mut current = dir.to_path_buf();
    loop {
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
