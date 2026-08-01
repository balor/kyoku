use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::{OrganizeOperation, Settings};
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

#[derive(Debug, Clone)]
pub struct CollectionCopyBackfill {
    pub track_id: i64,
    pub collection_id: i64,
    pub path: PathBuf,
}

/// Move/rename of an album's sibling cover-art file so it lands alongside
/// the tracks in their new album directory. Always moves (not copies) — the
/// source sits in whichever inbox/album directory the user imported from,
/// and that directory is typically being emptied anyway. Skipped when
/// `from == to` (already in place).
#[derive(Debug, Clone)]
pub struct CoverMove {
    pub album_id: i64,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Default)]
pub struct OrganizePlan {
    pub moves: Vec<FileMove>,
    pub copies: Vec<FileCopy>,
    /// DB-only repairs for collection copies that already exist at their
    /// rendered target but whose `collection_tracks.collection_file_path`
    /// is NULL/stale. Template-change moves of old collection copies are
    /// still a TODO; this only backfills the already-correct on-disk file.
    pub copy_backfills: Vec<CollectionCopyBackfill>,
    pub cover_moves: Vec<CoverMove>,
    pub skipped: usize,
    /// Tracks whose source file no longer exists on disk. These rows are
    /// orphaned — they point at paths that have been moved/deleted/renamed
    /// outside of kyoku. `apply_organize` will delete these DB rows.
    pub missing_sources: Vec<(i64, PathBuf, String)>,
    /// Files on disk whose DB row has already been removed (most often
    /// because an import replaced them via duplicate resolution). The
    /// file itself is still sitting in the music dir waiting to be
    /// deleted; `apply_organize` unlinks each file and clears the
    /// tracking row from `orphaned_files`.
    pub file_orphans: Vec<FileOrphanEntry>,
    /// Set when the missing-source count looks like an unavailable volume
    /// (unmounted drive leaving an empty mount point) rather than genuinely
    /// deleted files. While set, `apply_organize` refuses to prune the
    /// missing-source rows; everything else in the plan still applies.
    pub prune_blocked_reason: Option<String>,
}

/// One pending orphan file pulled from the `orphaned_files` table.
/// Keeps the identifying tag snapshot so the preview can show something
/// human-readable even though the track row is gone.
#[derive(Debug, Clone)]
pub struct FileOrphanEntry {
    pub id: i64,
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_title: Option<String>,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct OrganizeResult {
    pub moved: u32,
    pub copied: u32,
    pub covers_moved: u32,
    pub errors: Vec<(String, String)>,
    pub dirs_cleaned: u32,
    pub orphans_cleaned: u32,
    /// Number of `orphaned_files` entries fully handled (file deleted
    /// or already missing, tracking row cleared).
    pub file_orphans_removed: u32,
    /// Copied from the plan when the missing-source prune was skipped,
    /// so callers can surface it after apply.
    pub prune_blocked_reason: Option<String>,
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

/// Per-track template-rendering context. Built once per track and shared by
/// the reservation pre-pass and the assignment pass in [`plan_organize`] so
/// both compute identical raw target paths.
struct TrackTargets {
    from: PathBuf,
    has_album: bool,
    single_disc: bool,
    metadata_starved: bool,
    vars: TemplateVars,
    /// Memberships sorted by collection ID (oldest first) — deterministic
    /// primary-home selection for loose tracks.
    collections: Vec<queries::OrganizeCollectionMembership>,
}

impl TrackTargets {
    fn new(t: &queries::OrganizeTrackRow) -> Self {
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
            position: 0,
        };
        // Metadata-starved tracks (no track number, no artist, no album_artist)
        // would render as garbage like "00 Unknown - <stem>.ext" — keep the
        // original filename instead while still honouring the template's directory.
        let metadata_starved = t.track_number.unwrap_or(0) == 0
            && t.artist.as_deref().unwrap_or("").trim().is_empty()
            && t.album_artist.as_deref().unwrap_or("").trim().is_empty();
        let mut collections = t.collections.clone();
        collections.sort_by_key(|c| c.id);
        TrackTargets {
            from: PathBuf::from(&t.file_path),
            has_album: t.album_title.is_some(),
            single_disc: t.disc_total.unwrap_or(1) <= 1,
            metadata_starved,
            vars,
            collections,
        }
    }

    fn preserve_filename(&self, target: PathBuf) -> PathBuf {
        if !self.metadata_starved {
            return target;
        }
        match (target.parent(), self.from.file_name()) {
            (Some(dir), Some(name)) => dir.join(name),
            _ => target,
        }
    }

    /// Raw (pre-disambiguation) target for one collection membership.
    /// Collection templates use `{position}` for collection order while
    /// `{track}` stays the source track-number tag.
    fn collection_target(
        &self,
        coll: &queries::OrganizeCollectionMembership,
        settings: &Settings,
        music_dir: &Path,
    ) -> PathBuf {
        let tmpl = coll
            .path_template
            .as_deref()
            .unwrap_or(&settings.library.collection_path_template);
        let mut coll_vars = self.vars.clone();
        coll_vars.collection = coll.name.clone();
        coll_vars.position = coll.effective_position;
        self.preserve_filename(music_dir.join(template::render_path(tmpl, &coll_vars)))
    }

    /// Raw target of the track's PRIMARY file: album hierarchy for album
    /// tracks, first collection's folder for loose tracks in collections,
    /// the loose template otherwise.
    fn primary_target(&self, settings: &Settings, music_dir: &Path) -> PathBuf {
        if self.has_album {
            let tmpl = if self.single_disc {
                &settings.library.path_template_single_disc
            } else {
                &settings.library.path_template
            };
            self.preserve_filename(music_dir.join(template::render_path(tmpl, &self.vars)))
        } else if let Some(first) = self.collections.first() {
            self.collection_target(first, settings, music_dir)
        } else {
            self.preserve_filename(music_dir.join(template::render_path(
                &settings.library.loose_path_template,
                &self.vars,
            )))
        }
    }
}

fn maybe_backfill_collection_copy(
    plan: &mut OrganizePlan,
    track_id: i64,
    coll: &queries::OrganizeCollectionMembership,
    target: &Path,
) {
    let target_string = target.display().to_string();
    if coll.collection_file_path.as_deref() != Some(target_string.as_str()) {
        plan.copy_backfills.push(CollectionCopyBackfill {
            track_id,
            collection_id: coll.id,
            path: target.to_path_buf(),
        });
    }
}

/// Compute an organize plan without any side effects.
pub fn plan_organize(
    conn: &Connection,
    settings: &Settings,
    filter: OrganizeFilter,
) -> Result<OrganizePlan> {
    let music_dir = &settings.library.music_dir;

    // Refuse to plan against an unavailable library root. Every stored track
    // resolves below music_dir, so a missing/unreadable dir (unmounted drive,
    // or EACCES — where `exists()` also returns false) would classify the
    // ENTIRE library as missing sources, and apply would prune every row.
    // read_dir instead of exists() so permission failures are caught too.
    if let Err(e) = std::fs::read_dir(music_dir) {
        return Err(crate::error::KyokuError::Config(format!(
            "music directory {} is not accessible ({}) — is the drive mounted?",
            music_dir.display(),
            e
        )));
    }

    let mut plan = OrganizePlan::default();

    // Pending file orphans are a property of the library as a whole, not
    // of any particular (artist/album/collection) filter — an orphan
    // row has no album_id by the time we see it. Always include them so
    // running any organize pass eventually cleans them up.
    for o in queries::list_orphans(conn, music_dir)? {
        plan.file_orphans.push(FileOrphanEntry {
            id: o.id,
            path: PathBuf::from(o.file_path),
            title: o.title,
            artist: o.artist,
            album_title: o.album_title,
            reason: o.reason,
        });
    }

    let tracks = queries::get_all_tracks_for_organize(conn, music_dir, &filter)?;

    // Collision tracking: `tracks.file_path` has a UNIQUE constraint in the
    // DB, so two tracks cannot end up with the same destination path.
    // We start with every existing track path, remove the ones being moved
    // (those slots will be freed), and disambiguate proposed targets
    // against this set before committing them to the plan.
    use std::collections::{BTreeMap, HashSet};
    let mut used_paths: HashSet<PathBuf> = queries::list_all_track_paths(conn, music_dir)?
        .into_iter()
        .map(|(_, p)| PathBuf::from(p))
        .collect();

    // Record each album's target directory (parent of any of its tracks'
    // destinations). Used after the main loop to emit cover-art moves so the
    // sibling cover file follows its album into its new directory.
    let mut album_dest_dir: BTreeMap<i64, PathBuf> = BTreeMap::new();
    for t in &tracks {
        used_paths.remove(&PathBuf::from(&t.file_path));
    }

    // Also claim every collection copy recorded in the DB — a move target
    // could otherwise collide with another track's collection copy that
    // exists in the DB but not (yet) on disk. A planned track's own
    // membership slots are exempt: if its recorded copy is missing on disk,
    // the plan should re-copy to that same path, not get pushed to a " (2)"
    // variant of it.
    let planned_memberships: HashSet<(i64, i64)> = tracks
        .iter()
        .flat_map(|t| t.collections.iter().map(move |c| (t.id, c.id)))
        .collect();
    for (track_id, coll_id, p) in queries::list_all_collection_paths(conn, music_dir)? {
        if !planned_memberships.contains(&(track_id, coll_id)) {
            used_paths.insert(PathBuf::from(p));
        }
    }

    // Pending orphan paths are the one kind of on-disk file a move may land
    // on: dup-replace imports plan that overwrite deliberately, and apply
    // resolves it via its `occupied` guard.
    let orphan_paths: HashSet<PathBuf> = plan.file_orphans.iter().map(|e| e.path.clone()).collect();

    let same_file = |a: &Path, b: &Path| -> bool {
        matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
    };
    let slot_free = |candidate: &Path, from: &Path, used: &HashSet<PathBuf>| -> bool {
        if used.contains(candidate) {
            return false;
        }
        // A file already on disk blocks the slot — rename/copy would
        // silently clobber it. Exceptions: pending orphans (deliberate
        // overwrite, see above) and the track's own file under a different
        // spelling (case-only rename on a case-insensitive filesystem).
        !candidate.exists() || orphan_paths.contains(candidate) || same_file(candidate, from)
    };

    // Helper: return a variant of `target` whose slot is free — not claimed
    // by another planned destination (`used`) and not an unrelated file
    // already on disk. Appends " (2)", " (3)", … before the extension.
    // Inserts the final chosen path into `used` as a side effect.
    let disambiguate = |from: &Path, target: PathBuf, used: &mut HashSet<PathBuf>| -> PathBuf {
        if slot_free(&target, from, used) {
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
            if slot_free(&candidate, from, used) {
                used.insert(candidate.clone());
                return candidate;
            }
        }
        // Fallback: just return the original and let the DB complain
        used.insert(target.clone());
        target
    };

    // Build each track's render context (and primary target) up front.
    // `None` marks a missing source — detected here so the reservation
    // pre-pass below skips them, handled in the assignment loop.
    let contexts: Vec<Option<(TrackTargets, PathBuf)>> = tracks
        .iter()
        .map(|t| {
            let ctx = TrackTargets::new(t);
            if !ctx.from.exists() {
                return None;
            }
            let primary = ctx.primary_target(settings, music_dir);
            Some((ctx, primary))
        })
        .collect();

    // Reservation pre-pass: a track that is already at its rendered target
    // owns that slot, full stop. Claim those slots BEFORE assigning move
    // destinations so the outcome can't depend on SQL row order — without
    // this, a mover processed first could claim an in-place track's path
    // and apply would rename right over its file.
    for (ctx, primary) in contexts.iter().flatten() {
        if &ctx.from == primary {
            used_paths.insert(primary.clone());
        }
    }

    for (t, entry) in tracks.iter().zip(&contexts) {
        // Orphaned DB rows: tracks whose source file no longer exists.
        // These happen when a previous organize run partially succeeded and
        // left the DB pointing at gone files, or when the user deleted files
        // manually. Collected for pruning during apply (subject to the
        // prune_blocked_reason guard below).
        let Some((ctx, primary)) = entry else {
            plan.missing_sources
                .push((t.id, PathBuf::from(&t.file_path), t.title.clone()));
            continue;
        };
        let from = &ctx.from;

        if ctx.has_album {
            // Album track: move to album hierarchy + copy to each collection
            let raw_target = primary.clone();

            // Copies must read from the post-move location because apply_organize
            // runs moves before copies — by then `from` has been renamed.
            let copy_source: PathBuf = if from == &raw_target {
                // Already in place — slot was claimed in the pre-pass.
                plan.skipped += 1;
                raw_target
            } else {
                let target = disambiguate(from, raw_target, &mut used_paths);
                if from == &target {
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

            // Record the album dir for later cover-move planning. Entered once
            // per album — first track wins (all tracks share the same dir).
            if let Some(aid) = t.album_id
                && let Some(parent) = copy_source.parent()
            {
                album_dest_dir
                    .entry(aid)
                    .or_insert_with(|| parent.to_path_buf());
            }

            // One copy per collection — skip if the target already exists on disk
            // (collection was already organized), but repair the DB pointer if
            // it is missing or stale.
            for coll in &ctx.collections {
                let raw = ctx.collection_target(coll, settings, music_dir);
                if raw.exists() {
                    maybe_backfill_collection_copy(&mut plan, t.id, coll, &raw);
                    used_paths.insert(raw);
                    plan.skipped += 1;
                } else {
                    let target = disambiguate(&copy_source, raw, &mut used_paths);
                    plan.copies.push(FileCopy {
                        track_id: t.id,
                        collection_id: coll.id,
                        collection_name: coll.name.clone(),
                        from: copy_source.clone(),
                        to: target,
                    });
                }
            }
        } else if !ctx.collections.is_empty() {
            // Loose track in collections: MOVE to first collection's folder
            let first = &ctx.collections[0];
            let raw_primary = primary.clone();

            // Same post-move-location rule as above.
            let copy_source: PathBuf = if from == &raw_primary {
                plan.skipped += 1;
                raw_primary
            } else {
                let primary_target = disambiguate(from, raw_primary, &mut used_paths);
                if from == &primary_target {
                    plan.skipped += 1;
                    primary_target
                } else {
                    plan.moves.push(FileMove {
                        track_id: t.id,
                        from: from.clone(),
                        to: primary_target.clone(),
                        also_collection: Some((first.id, first.name.clone())),
                    });
                    primary_target
                }
            };

            // COPY to each additional collection's folder — skip if already exists,
            // but repair the DB pointer if it is missing or stale.
            for coll in &ctx.collections[1..] {
                let raw = ctx.collection_target(coll, settings, music_dir);
                if raw.exists() {
                    maybe_backfill_collection_copy(&mut plan, t.id, coll, &raw);
                    used_paths.insert(raw);
                    plan.skipped += 1;
                } else {
                    let target = disambiguate(&copy_source, raw, &mut used_paths);
                    plan.copies.push(FileCopy {
                        track_id: t.id,
                        collection_id: coll.id,
                        collection_name: coll.name.clone(),
                        from: copy_source.clone(),
                        to: target,
                    });
                }
            }
        } else {
            // Loose track, no collections: move to _loose/ folder
            let raw_target = primary.clone();
            if from == &raw_target {
                plan.skipped += 1;
            } else {
                let target = disambiguate(from, raw_target, &mut used_paths);
                if from == &target {
                    plan.skipped += 1;
                } else {
                    plan.moves.push(FileMove {
                        track_id: t.id,
                        from: from.clone(),
                        to: target,
                        also_collection: None,
                    });
                }
            }
        }
    }

    // Prune guard: when a large share of the candidate tracks are "missing",
    // the likely cause is an unavailable volume (an unmounted drive leaves an
    // empty mount point that passes the read_dir check above), not a mass
    // deletion. Block the prune step; moves/copies for files that ARE present
    // still apply. Genuinely gone files can be removed via the explicit
    // delete flows, or organize re-run once the volume is back.
    let missing = plan.missing_sources.len();
    if missing >= 100 || (missing >= 5 && missing * 5 > tracks.len()) {
        plan.prune_blocked_reason = Some(format!(
            "{} of {} source files are missing — this looks like an unavailable \
             volume, so their DB rows will NOT be pruned",
            missing,
            tracks.len()
        ));
    }

    // Cover-art moves: for every album with a recorded destination dir and a
    // sibling cover file stamped on the row, schedule a move to
    // `<album_dir>/cover.<ext>`. Skipped when source == dest or the source
    // file no longer exists on disk.
    for (album_id, album_dir) in &album_dest_dir {
        let Some(src_str) = queries::get_album_cover_path(conn, music_dir, *album_id)? else {
            continue;
        };
        let src = PathBuf::from(&src_str);
        if !src.exists() {
            continue;
        }
        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("jpg")
            .to_ascii_lowercase();
        let dest = album_dir.join(format!("cover.{}", ext));
        if src == dest {
            continue;
        }
        plan.cover_moves.push(CoverMove {
            album_id: *album_id,
            from: src,
            to: dest,
        });
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
    music_dir: &Path,
    plan: &OrganizePlan,
    operation: OrganizeOperation,
    cleanup_roots: &[PathBuf],
) -> Result<OrganizeResult> {
    let mut result = OrganizeResult::default();
    let mut emptied_dirs: Vec<PathBuf> = Vec::new();
    // Destination paths claimed by moves/copies/cover-moves during this
    // apply. Used below to detect "orphan path == freshly-moved file"
    // which happens after a dup-replace import where the new file lands
    // at the same library location as the orphan we were about to unlink.
    // Both literal and canonicalized forms are inserted so NFC/NFD and
    // symlink variants on macOS don't slip past the guard.
    let mut occupied: HashSet<String> = HashSet::new();

    // Orphan destinations are the one kind of existing file a move may
    // overwrite (dup-replace flow). Pre-expand literal + canonical forms,
    // mirroring `mark_occupied`.
    let mut orphan_exempt: HashSet<String> = HashSet::new();
    for e in &plan.file_orphans {
        set_insert(&mut orphan_exempt, e.path.display().to_string());
        if let Ok(canon) = std::fs::canonicalize(&e.path) {
            set_insert(&mut orphan_exempt, canon.display().to_string());
        }
    }
    let same_file = |a: &Path, b: &Path| -> bool {
        matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
    };
    let path_in = |p: &Path, set: &HashSet<String>| -> bool {
        set_has(set, &p.display().to_string())
            || std::fs::canonicalize(p)
                .map(|c| set_has(set, &c.display().to_string()))
                .unwrap_or(false)
    };

    for m in &plan.moves {
        // Backstop for stale plans: destinations were vetted against disk at
        // plan time, but a file may have appeared since. Never clobber it —
        // error and continue. Exceptions match plan-time `slot_free`: pending
        // orphans (deliberate overwrite) and the source file itself under a
        // different spelling (case-only rename on case-insensitive fs).
        if m.to.exists() && !path_in(&m.to, &orphan_exempt) && !same_file(&m.from, &m.to) {
            result.errors.push((
                m.from.display().to_string(),
                format!(
                    "destination {} already exists — skipped to avoid overwriting it",
                    m.to.display()
                ),
            ));
            continue;
        }
        if let Some(parent) = m.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let outcome = if matches!(operation, OrganizeOperation::Copy) {
            std::fs::copy(&m.from, &m.to).map(|_| ())
        } else {
            move_file(&m.from, &m.to)
        };

        match outcome {
            Ok(()) => {
                let new_path = m.to.display().to_string();

                // Update tracks.file_path. On failure, record a detailed error
                // (file is already on disk at `new_path`) and skip this track's
                // remaining updates — but keep processing the rest of the plan.
                if let Err(e) = queries::update_track_path(conn, music_dir, m.track_id, &new_path) {
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
                        conn, music_dir, *coll_id, m.track_id, &new_path,
                    )
                {
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
                mark_occupied(&m.to, &mut occupied);

                // Track the source directory for cleanup
                if !matches!(operation, OrganizeOperation::Copy)
                    && let Some(parent) = m.from.parent()
                {
                    emptied_dirs.push(parent.to_path_buf());
                }
            }
            Err(e) => {
                result
                    .errors
                    .push((m.from.display().to_string(), e.to_string()));
            }
        }
    }

    for backfill in &plan.copy_backfills {
        let new_path = backfill.path.display().to_string();
        if let Err(e) = queries::update_collection_track_path(
            conn,
            music_dir,
            backfill.collection_id,
            backfill.track_id,
            &new_path,
        ) {
            result.errors.push((
                new_path,
                format!("collection copy exists but DB backfill failed: {e}"),
            ));
        }
    }

    for c in &plan.copies {
        // Same stale-plan backstop as moves. Copies are never planned onto
        // an existing file (plan skips those), so anything here is a race.
        if c.to.exists() && !same_file(&c.from, &c.to) {
            result.errors.push((
                c.from.display().to_string(),
                format!(
                    "copy destination {} already exists — skipped",
                    c.to.display()
                ),
            ));
            continue;
        }
        if let Some(parent) = c.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        match std::fs::copy(&c.from, &c.to) {
            Ok(_) => {
                let new_path = c.to.display().to_string();
                if let Err(e) = queries::update_collection_track_path(
                    conn,
                    music_dir,
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
                mark_occupied(&c.to, &mut occupied);
            }
            Err(e) => {
                result
                    .errors
                    .push((c.from.display().to_string(), e.to_string()));
            }
        }
    }

    // Cover-art moves: run after audio so album destination dirs already
    // exist. Each cover failure is recorded per-album; the rest continue.
    for cm in &plan.cover_moves {
        if let Some(parent) = cm.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let outcome = if matches!(operation, OrganizeOperation::Copy) {
            std::fs::copy(&cm.from, &cm.to).map(|_| ())
        } else {
            move_file(&cm.from, &cm.to)
        };
        match outcome {
            Ok(()) => {
                let new_path = cm.to.display().to_string();
                if let Err(e) =
                    queries::set_album_cover_path(conn, music_dir, cm.album_id, &new_path)
                {
                    result.errors.push((
                        cm.from.display().to_string(),
                        format!("cover moved to {new_path} but DB update failed: {e}"),
                    ));
                    continue;
                }
                result.covers_moved += 1;
                mark_occupied(&cm.to, &mut occupied);
                if !matches!(operation, OrganizeOperation::Copy)
                    && let Some(parent) = cm.from.parent()
                {
                    emptied_dirs.push(parent.to_path_buf());
                }
            }
            Err(e) => {
                result
                    .errors
                    .push((cm.from.display().to_string(), e.to_string()));
            }
        }
    }

    // Prune orphaned DB rows (tracks whose source file no longer exists) —
    // unless planning flagged the batch as a probably-unavailable volume.
    if let Some(reason) = &plan.prune_blocked_reason {
        result.prune_blocked_reason = Some(reason.clone());
    } else {
        for (track_id, path, title) in &plan.missing_sources {
            // Re-check at apply time: a remount or restore between plan and
            // apply means the row is live again — keep it.
            if path.exists() {
                continue;
            }
            match queries::delete_track(conn, *track_id) {
                Ok(()) => result.orphans_cleaned += 1,
                Err(e) => result.errors.push((
                    path.display().to_string(),
                    format!("failed to prune DB row for missing '{}': {}", title, e),
                )),
            }
        }
    }

    // Delete pending file orphans (files on disk whose track row was
    // already removed — typically dup replacements from import). If the
    // file is already gone, we still clear the tracking row (idempotent
    // cleanup). Parent dirs of unlinked orphans get the same emptied-
    // dir treatment so a stranded album directory collapses cleanly.
    for entry in &plan.file_orphans {
        // CRITICAL: if this orphan path was just claimed by a successful
        // move/copy/cover-move, the file at that path is now the new
        // live track — unlinking it here would destroy freshly-imported
        // audio (this exact footgun caused missing files after a dup
        // "Keep New" import + organize). The orphan is considered
        // resolved (replaced on disk), so we still clear the tracking
        // row, just skip the filesystem delete.
        let orphan_literal = entry.path.display().to_string();
        let orphan_canon = std::fs::canonicalize(&entry.path)
            .ok()
            .map(|p| p.display().to_string());
        let replaced_by_move = set_has(&occupied, &orphan_literal)
            || orphan_canon
                .as_ref()
                .map(|s| set_has(&occupied, s))
                .unwrap_or(false);

        let unlink_ok = if replaced_by_move {
            tracing::info!(
                "orphan {} was overwritten by a move in this apply — skipping unlink",
                entry.path.display()
            );
            true
        } else {
            match std::fs::remove_file(&entry.path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(e) => {
                    result.errors.push((
                        entry.path.display().to_string(),
                        format!("orphan unlink failed: {}", e),
                    ));
                    false
                }
            }
        };
        if !unlink_ok {
            continue;
        }
        if let Err(e) = queries::delete_orphan(conn, entry.id) {
            // File is gone, row couldn't be removed — surface it but
            // don't count as removed; a re-run will try again.
            result.errors.push((
                entry.path.display().to_string(),
                format!("orphan row delete failed: {}", e),
            ));
            continue;
        }
        result.file_orphans_removed += 1;
        // Don't schedule the parent for emptied-dir cleanup when the
        // orphan was replaced by a move — the directory is now hosting
        // the new file, not empty.
        if !replaced_by_move && let Some(parent) = entry.path.parent() {
            emptied_dirs.push(parent.to_path_buf());
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

/// Record a destination path as "now occupied by a live file". We insert
/// both the literal path string and its canonical form so orphan
/// comparisons below catch NFC/NFD and symlink variants — APFS normalizes
/// filenames to NFD while tags/DB may hold NFC, and `canonicalize` has
/// bitten us before on macOS path matching.
fn mark_occupied(p: &Path, occupied: &mut HashSet<String>) {
    set_insert(occupied, p.display().to_string());
    if let Ok(canon) = std::fs::canonicalize(p) {
        set_insert(occupied, canon.display().to_string());
    }
}

/// Case-folded spelling of a path for occupied-set membership — only on
/// Windows, where NTFS is case-insensitive by default and two planned
/// destinations like `Track.mp3`/`track.mp3` that don't exist yet would
/// otherwise both pass the "slot free" check and collide at write time.
/// Deliberately NOT folded on case-sensitive filesystems (Linux ext4),
/// where "A.mp3" and "a.mp3" legitimately coexist and folding would
/// report phantom collisions.
#[cfg(windows)]
fn folded(s: &str) -> Option<String> {
    Some(s.to_lowercase())
}

/// Case-folding is a Windows-only concept; see [`folded`].
#[cfg(not(windows))]
fn folded(_: &str) -> Option<String> {
    None
}

/// Occupied-set insert that also records the case-folded spelling on
/// Windows (see [`folded`]).
fn set_insert(set: &mut HashSet<String>, s: String) {
    if let Some(f) = folded(&s) {
        set.insert(f);
    }
    set.insert(s);
}

/// Occupied-set membership that also matches the case-folded spelling
/// on Windows (see [`folded`]).
fn set_has(set: &HashSet<String>, s: &str) -> bool {
    set.contains(s) || folded(s).is_some_and(|f| set.contains(&f))
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(from, to).map(|_| ())?;
            std::fs::remove_file(from)
        }
        result => result,
    }
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
#[allow(dead_code)]
pub fn plan_delete_collection(
    conn: &Connection,
    collection_id: i64,
    music_dir: &Path,
) -> Result<DeleteCollectionPlan> {
    plan_delete_collection_with_roots(conn, music_dir, collection_id, &[music_dir.to_path_buf()])
}

pub fn plan_delete_collection_with_roots(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
    managed_roots: &[PathBuf],
) -> Result<DeleteCollectionPlan> {
    let homes = queries::get_collection_tracks_with_other_homes(conn, music_dir, collection_id)?;
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
            if path_is_in_roots(&p, managed_roots) {
                files_to_delete.push(p);
            } else {
                files_outside_music_dir.push(p);
            }
        }

        // Determine if this track has any other home. If not, deleting the
        // collection removes the track from the library by default; optional
        // file deletion should include its primary file too, even when no
        // organized collection copy exists yet.
        let has_other_home = h.has_album || h.other_collection_count > 0;
        if !has_other_home {
            orphaned_track_ids.push(h.track_id);
            if !h.track_file_path.is_empty()
                && h.collection_file_path.as_deref() != Some(h.track_file_path.as_str())
            {
                let p = PathBuf::from(&h.track_file_path);
                if path_is_in_roots(&p, managed_roots) {
                    files_to_delete.push(p);
                } else {
                    files_outside_music_dir.push(p);
                }
            }
            continue;
        }

        // Track has another home — if its tracks.file_path matches the
        // about-to-be-deleted collection file, we need to promote another path.
        if let Some(coll_path) = &h.collection_file_path
            && &h.track_file_path == coll_path
            && !h.has_album
        {
            // No album to fall back on, but other collection(s) exist.
            // Find one existing collection_file_path value to promote.
            if let Some(new_path) =
                find_other_collection_path(conn, music_dir, h.track_id, collection_id)?
            {
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
    music_dir: &Path,
    track_id: i64,
    exclude_collection_id: i64,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT collection_file_path FROM collection_tracks
         WHERE track_id = ?1 AND collection_id != ?2
            AND collection_file_path IS NOT NULL",
    )?;
    let rows = stmt.query_map(rusqlite::params![track_id, exclude_collection_id], |row| {
        row.get::<_, String>(0)
    })?;
    for row in rows {
        let stored = row?;
        let path = crate::core::paths::from_db_path(&stored, music_dir);
        if path.exists() {
            // promote_paths carries absolute paths (rest of the codebase deals in
            // absolute and the `update_track_path` boundary re-normalises).
            return Ok(Some(path.display().to_string()));
        }
    }
    Ok(None)
}

/// Execute a collection deletion plan. Orphaned tracks are removed from the
/// library by default so deleting a collection never silently creates loose
/// tracks. If `delete_files` is true, also removes eligible physical files.
#[allow(dead_code)]
pub fn apply_delete_collection(
    conn: &Connection,
    music_dir: &Path,
    plan: &DeleteCollectionPlan,
    delete_files: bool,
) -> Result<DeleteCollectionResult> {
    apply_delete_collection_with_roots(
        conn,
        music_dir,
        plan,
        delete_files,
        &[music_dir.to_path_buf()],
    )
}

pub fn apply_delete_collection_with_roots(
    conn: &Connection,
    music_dir: &Path,
    plan: &DeleteCollectionPlan,
    delete_files: bool,
    cleanup_roots: &[PathBuf],
) -> Result<DeleteCollectionResult> {
    let mut result = DeleteCollectionResult::default();
    let mut emptied_dirs: Vec<PathBuf> = Vec::new();

    if delete_files {
        // Delete physical files (only those inside music_dir)
        for p in &plan.files_to_delete {
            if !p.exists() {
                continue;
            }
            if !path_is_in_roots(p, cleanup_roots) {
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

    // Promote alternate file_paths for tracks that survive only when the
    // current primary file was actually deleted. With delete_files=false the
    // primary file stays on disk and should remain canonical.
    if delete_files {
        for (track_id, new_path) in &plan.promote_paths {
            if let Err(e) = queries::update_track_path(conn, music_dir, *track_id, new_path) {
                result.errors.push((
                    new_path.clone(),
                    format!("failed to promote surviving track path: {e}"),
                ));
            }
        }
    }

    // Delete orphaned tracks entirely so they do not silently become loose.
    for &track_id in &plan.orphaned_track_ids {
        match queries::delete_track(conn, track_id) {
            Ok(()) => result.tracks_orphaned_removed += 1,
            Err(e) => result.errors.push((
                format!("track #{track_id}"),
                format!("failed to delete orphaned track row: {e}"),
            )),
        }
    }

    // Finally, drop the collection (cascade removes collection_tracks rows)
    if let Err(e) = queries::delete_collection(conn, plan.collection_id) {
        result.errors.push((
            plan.collection_name.clone(),
            format!("failed to delete collection row: {e}"),
        ));
    }

    // Clean up empty parent dirs
    if delete_files {
        emptied_dirs.sort();
        emptied_dirs.dedup();
        emptied_dirs.reverse();
        for dir in &emptied_dirs {
            result.dirs_cleaned += remove_empty_parents(dir, cleanup_roots);
        }
    }

    Ok(result)
}

fn path_is_in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    crate::core::paths::path_is_strictly_inside(path, roots)
}

/// Directory roots where Kyoku may delete tracked music files on explicit
/// user confirmation. This is intentionally narrower than `cleanup_roots`:
/// import/inbox folders are staging areas and should not make files outside
/// the library eligible for the destructive "delete from disk" option.
pub fn file_delete_roots(settings: &Settings) -> Vec<PathBuf> {
    vec![settings.library.music_dir.clone()]
}

/// Managed directory roots for directory-cleanup safety: music_dir plus any
/// configured inbox dirs. `apply_organize` uses these as a whitelist when
/// sweeping empty parents — nothing outside this set is ever touched.
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
    if !crate::core::paths::path_is_strictly_inside(dir, roots) {
        return 0;
    }

    let mut cleaned = 0u32;
    let mut current = dir.to_path_buf();
    loop {
        // Never touch a path that is not strictly inside a managed root.
        if !crate::core::paths::path_is_strictly_inside(&current, roots) {
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

#[cfg(test)]
#[path = "organizer_tests.rs"]
mod tests;
