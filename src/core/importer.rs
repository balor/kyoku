use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use walkdir::WalkDir;

use crate::core::collection_order::{self, CollectionOrderItem};
use crate::core::tagger;
use crate::db::models::{SUPPORTED_EXTENSIONS, Track};
use crate::db::queries;
use crate::error::Result;

/// Basenames we accept for sibling cover art (matched case-insensitively,
/// extension-less). First hit wins — order matters.
const COVER_BASENAMES: &[&str] = &["cover", "folder", "front", "artwork", "album", "albumart"];

/// Allowed cover-image extensions (matched case-insensitively).
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// Look for a cover-art file sitting next to the audio files inside `dir`.
/// Returns the first match found, checking `COVER_BASENAMES` in order and
/// pairing each with any of `COVER_EXTS`. Basename and extension are matched
/// case-insensitively.
///
/// Non-recursive: only entries directly in `dir` are considered.
pub fn detect_sibling_cover(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .collect();

    for wanted_base in COVER_BASENAMES {
        for entry in &entries {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !stem.eq_ignore_ascii_case(wanted_base) {
                continue;
            }
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            let ext_lower = ext.to_ascii_lowercase();
            if COVER_EXTS.iter().any(|e| *e == ext_lower) {
                return Some(path);
            }
        }
    }
    None
}

/// Result of an import operation.
#[derive(Debug, Default)]
pub struct ImportResult {
    pub imported: u32,
    pub skipped_duplicate: u32,
    pub skipped_error: u32,
    pub added_to_collection: u32,
    pub albums_created: u32,
    pub albums_existing: u32,
    pub collection_created: bool,
    pub errors: Vec<(String, String)>,
}

/// Scan a directory for audio files, returning absolute paths.
pub fn scan_audio_files(path: impl AsRef<Path>) -> Vec<std::path::PathBuf> {
    let path = path.as_ref();
    let mut files = Vec::new();

    for entry in WalkDir::new(path).follow_links(true).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            files.push(entry.into_path());
        }
    }

    files.sort();
    files
}

/// Group tracks by their album (using album tag + album_artist + directory).
/// Returns a map of group key -> list of tracks.
/// If `loose` is true, each track is its own group (no album grouping).
fn group_into_albums(tracks: &[Track], loose: bool) -> HashMap<String, Vec<usize>> {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, track) in tracks.iter().enumerate() {
        let key = if loose {
            // Each track is independent
            format!("__loose__{}", i)
        } else {
            // Group by album tag + directory
            let album = track
                .file_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown");

            // If we have album tag from reading, prefer that
            // But we don't have album on Track — we'll use source_dir as grouping key
            track
                .source_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| album.to_string())
        };

        groups.entry(key).or_default().push(i);
    }

    groups
}

fn ordered_group_indices(
    indices: &[usize],
    tracks: &[Track],
    tag_data_map: &[Option<tagger::TagData>],
) -> Vec<usize> {
    let items: Vec<CollectionOrderItem> = indices
        .iter()
        .enumerate()
        .map(|(order, &idx)| {
            let tag = tag_data_map.get(idx).and_then(|td| td.as_ref());
            let album_key = tag.and_then(|td| {
                td.album.as_ref().map(|album| {
                    format!(
                        "{}\u{0}{}",
                        td.album_artist
                            .as_deref()
                            .or(td.artist.as_deref())
                            .unwrap_or(""),
                        album
                    )
                })
            });
            CollectionOrderItem {
                index: idx,
                track_id: idx as i64,
                explicit_position: None,
                disc_number: tracks[idx].disc_number,
                track_number: tracks[idx].track_number,
                album_id: None,
                album_key,
                added_order: order,
                title: tracks[idx].title.clone(),
            }
        })
        .collect();
    collection_order::ordered_indices(&items)
}

/// Import audio files from a path into the database.
///
/// - Scans for audio files
/// - Reads tags
/// - Groups into albums (unless `loose`)
/// - Inserts into SQLite
/// - Optionally adds to a collection
///
/// `music_dir` is the library root; paths under it get stored relative.
pub fn import(
    conn: &Connection,
    music_dir: &Path,
    path: impl AsRef<Path>,
    loose: bool,
    pretend: bool,
    collection: Option<&str>,
) -> Result<ImportResult> {
    let path = path.as_ref();
    let mut result = ImportResult::default();

    // Scan for audio files
    let files = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        scan_audio_files(path)
    };

    if files.is_empty() {
        tracing::info!("No audio files found in {}", path.display());
        return Ok(result);
    }

    tracing::info!("Found {} audio file(s)", files.len());

    // Read tags for all files
    let mut tracks: Vec<Track> = Vec::new();
    let mut tag_data_map: Vec<Option<tagger::TagData>> = Vec::new();
    let mut duplicate_track_ids: Vec<(i64, String)> = Vec::new();

    for file_path in &files {
        let abs_path = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone());
        let path_str = abs_path.display().to_string();

        // Check for duplicates
        if queries::track_exists_by_path(conn, music_dir, &path_str)? {
            if collection.is_some() {
                if let Some(track_id) = queries::get_track_id_by_path(conn, music_dir, &path_str)? {
                    duplicate_track_ids.push((track_id, file_path.display().to_string()));
                }
            } else {
                tracing::info!("skip (already imported): {}", file_path.display());
            }
            result.skipped_duplicate += 1;
            continue;
        }

        match tagger::read_track(&abs_path) {
            Ok(mut track) => {
                track.file_path = abs_path;
                let tag_data = tagger::read_tags(file_path).ok();
                tag_data_map.push(tag_data);
                tracks.push(track);
            }
            Err(e) => {
                tracing::warn!("error reading {}: {}", file_path.display(), e);
                result
                    .errors
                    .push((file_path.display().to_string(), e.to_string()));
                result.skipped_error += 1;
            }
        }
    }

    // Ensure collection exists if requested
    let collection_id = if let Some(name) = collection {
        if !pretend {
            let (id, created) = queries::get_or_create_collection(conn, name)?;
            result.collection_created = created;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    // Add existing duplicate tracks to the collection
    if let Some(coll_id) = collection_id {
        for (track_id, path) in &duplicate_track_ids {
            if queries::add_track_to_collection(conn, coll_id, *track_id)? {
                tracing::info!("added to collection: {}", path);
                result.added_to_collection += 1;
            } else {
                tracing::info!("skip (already in collection): {}", path);
            }
        }
    }

    if tracks.is_empty() {
        if result.added_to_collection == 0 {
            tracing::info!("No new tracks to import.");
        }
        return Ok(result);
    }

    // Group into albums
    let groups = group_into_albums(&tracks, loose);

    if pretend {
        tracing::info!("Dry run — would import {} track(s):", tracks.len());
        for (key, indices) in &groups {
            if loose {
                for &i in indices {
                    tracing::info!("  [loose] {}", tracks[i].title);
                }
            } else {
                let album_name = tag_data_map
                    .get(indices[0])
                    .and_then(|td| td.as_ref())
                    .and_then(|td| td.album.as_deref())
                    .unwrap_or(key);
                tracing::info!("  Album: {} ({} tracks)", album_name, indices.len());
                for &i in indices {
                    tracing::info!("    - {}", tracks[i].title);
                }
            }
        }
        return Ok(result);
    }

    // Insert into database. HashMap iteration is intentionally avoided here:
    // collection positions should follow scan/group order, not a randomized
    // map order.
    let tx = conn.unchecked_transaction()?;
    let mut ordered_groups: Vec<&Vec<usize>> = groups.values().collect();
    ordered_groups.sort_by_key(|indices| indices.first().copied().unwrap_or(usize::MAX));

    for indices in ordered_groups {
        let ordered_indices = ordered_group_indices(indices, &tracks, &tag_data_map);

        // Create album if not loose and we have album info
        let album_id = if !loose {
            let first_idx = ordered_indices.first().copied().unwrap_or(indices[0]);
            let first_tag = tag_data_map.get(first_idx).and_then(|td| td.as_ref());

            if let Some(tag_data) = first_tag {
                if let Some(album_title) = &tag_data.album {
                    let (id, created) = queries::get_or_create_album(
                        &tx,
                        album_title,
                        tag_data.album_artist.as_deref(),
                        tag_data.year.map(|y| y as i32),
                        tag_data.genre.as_deref(),
                        indices.len() as u32,
                    )?;
                    if created {
                        result.albums_created += 1;
                    } else {
                        result.albums_existing += 1;
                    }
                    // Sibling-cover detection: scan the album source dir for a
                    // cover file and record its path. Only stamps when a file
                    // is found; organizer will later move it alongside audio.
                    if let Some(source_dir) = tracks[first_idx].source_dir.as_deref()
                        && let Some(cover) = detect_sibling_cover(source_dir)
                    {
                        queries::set_album_cover_path(
                            &tx,
                            music_dir,
                            id,
                            &cover.display().to_string(),
                        )?;
                    }
                    Some(id)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut inserted_ids = Vec::new();
        for i in ordered_indices {
            let track = &tracks[i];
            let file_size = std::fs::metadata(&track.file_path)
                .map(|m| m.len() as i64)
                .ok();

            let track_id = queries::insert_track(&tx, music_dir, track, album_id, file_size)?;
            inserted_ids.push(track_id);
            result.imported += 1;

            tracing::info!("imported: {}", track.title);
        }

        // Add to collection if requested, preserving the ordered import/group
        // sequence as collection positions.
        if let Some(coll_id) = collection_id {
            queries::add_tracks_to_collection_ordered(&tx, coll_id, &inserted_ids)?;
        }
    }

    tx.commit()?;

    Ok(result)
}

/// Scan inbox directories for audio files the DB doesn't yet know about.
/// "Knows about" is broader than just `tracks.file_path`: a file is excluded
/// if it's referenced by any track, by any `collection_tracks.collection_file_path`,
/// or by the `orphaned_files` table. This matters when one of the scan paths
/// is `music_dir` itself — we don't want to resurface collection copies or
/// pending-orphan leftovers as "untracked".
pub fn scan_inbox(
    conn: &Connection,
    music_dir: &Path,
    inbox_dirs: &[std::path::PathBuf],
) -> Result<Vec<std::path::PathBuf>> {
    // Build the exclusion set once per call — O(N) memory in library size,
    // but a single pass per table beats a per-file query.
    let known = queries::list_all_known_paths(conn, music_dir)?;
    let mut unimported = Vec::new();

    for dir in inbox_dirs {
        if !dir.exists() {
            tracing::warn!("inbox dir not found: {}", dir.display());
            continue;
        }

        let files = scan_audio_files(dir);
        for file_path in files {
            let abs_path = std::fs::canonicalize(&file_path).unwrap_or(file_path);
            let path_str = abs_path.display().to_string();
            if known.contains(&path_str) {
                continue;
            }
            unimported.push(abs_path);
        }
    }

    Ok(unimported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn fixtures_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_library")
    }

    fn no_root() -> &'static Path {
        Path::new("")
    }

    #[test]
    fn test_scan_audio_files() {
        let files = scan_audio_files(fixtures_dir());
        // We have tagged.mp3, no_title.mp3, cjk_tagged.mp3 (not_audio.txt is excluded)
        assert!(files.len() >= 3);
        assert!(files.iter().all(|f| {
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str())
        }));
    }

    #[test]
    fn test_import_basic() {
        let conn = db::open_memory().unwrap();
        let result = import(&conn, no_root(), fixtures_dir(), false, false, None).unwrap();
        assert!(result.imported >= 3);
        assert_eq!(result.skipped_duplicate, 0);
    }

    #[test]
    fn test_import_duplicates_skipped() {
        let conn = db::open_memory().unwrap();
        // Import once
        import(&conn, no_root(), fixtures_dir(), false, false, None).unwrap();
        // Import again — should skip all
        let result = import(&conn, no_root(), fixtures_dir(), false, false, None).unwrap();
        assert_eq!(result.imported, 0);
        assert!(result.skipped_duplicate >= 3);
    }

    #[test]
    fn test_import_loose() {
        let conn = db::open_memory().unwrap();
        let result = import(&conn, no_root(), fixtures_dir(), true, false, None).unwrap();
        assert!(result.imported >= 3);
        assert_eq!(result.albums_created, 0);
    }

    #[test]
    fn test_import_pretend() {
        let conn = db::open_memory().unwrap();
        let result = import(&conn, no_root(), fixtures_dir(), false, true, None).unwrap();
        assert_eq!(result.imported, 0); // pretend doesn't insert
    }

    #[test]
    fn test_import_with_collection() {
        let conn = db::open_memory().unwrap();
        let result = import(
            &conn,
            no_root(),
            fixtures_dir(),
            true,
            false,
            Some("Test Collection"),
        )
        .unwrap();
        assert!(result.imported >= 3);

        // Verify collection was created and tracks added
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM collection_tracks ct
                 JOIN collections c ON c.id = ct.collection_id
                 WHERE c.name = 'Test Collection'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count >= 3);
    }

    #[test]
    fn test_reimport_with_collection_adds_existing_tracks() {
        let conn = db::open_memory().unwrap();
        // Import once without collection
        let result1 = import(&conn, no_root(), fixtures_dir(), true, false, None).unwrap();
        assert!(result1.imported >= 3);

        // Re-import with collection — duplicates should be added to collection
        let result2 = import(
            &conn,
            no_root(),
            fixtures_dir(),
            true,
            false,
            Some("Favorites"),
        )
        .unwrap();
        assert_eq!(result2.imported, 0);
        assert!(result2.skipped_duplicate >= 3);
        assert!(result2.added_to_collection >= 3);
        assert!(result2.collection_created);

        // Verify tracks are in the collection
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM collection_tracks ct
                 JOIN collections c ON c.id = ct.collection_id
                 WHERE c.name = 'Favorites'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count >= 3);
    }

    #[test]
    fn detect_sibling_cover_picks_first_basename_hit() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Multiple candidates live side by side. `cover` beats `folder` in
        // our preference order, and the extension hit order follows file
        // discovery (we just assert a `cover.*` file wins).
        std::fs::write(tmp.path().join("folder.png"), b"").unwrap();
        std::fs::write(tmp.path().join("cover.jpg"), b"").unwrap();
        std::fs::write(tmp.path().join("song.mp3"), b"").unwrap();

        let found = detect_sibling_cover(tmp.path()).unwrap();
        assert_eq!(
            found.file_name().and_then(|s| s.to_str()),
            Some("cover.jpg")
        );
    }

    #[test]
    fn detect_sibling_cover_is_case_insensitive() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Front.JPEG"), b"").unwrap();

        let found = detect_sibling_cover(tmp.path()).unwrap();
        assert_eq!(
            found.file_name().and_then(|s| s.to_str()),
            Some("Front.JPEG")
        );
    }

    #[test]
    fn detect_sibling_cover_accepts_all_extensions() {
        for ext in ["jpg", "jpeg", "png", "webp"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let name = format!("albumart.{}", ext);
            std::fs::write(tmp.path().join(&name), b"").unwrap();
            let found = detect_sibling_cover(tmp.path()).unwrap();
            assert_eq!(
                found.file_name().and_then(|s| s.to_str()),
                Some(name.as_str())
            );
        }
    }

    #[test]
    fn detect_sibling_cover_returns_none_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("song.mp3"), b"").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"").unwrap();
        assert!(detect_sibling_cover(tmp.path()).is_none());
    }

    #[test]
    fn detect_sibling_cover_ignores_unknown_basenames() {
        let tmp = tempfile::TempDir::new().unwrap();
        // `random.jpg` is not in COVER_BASENAMES so it must be skipped.
        std::fs::write(tmp.path().join("random.jpg"), b"").unwrap();
        assert!(detect_sibling_cover(tmp.path()).is_none());
    }

    #[test]
    fn test_scan_inbox_empty() {
        let conn = db::open_memory().unwrap();
        let unimported = scan_inbox(&conn, no_root(), &[fixtures_dir()]).unwrap();
        assert!(unimported.len() >= 3);
    }

    #[test]
    fn test_scan_inbox_after_import() {
        let conn = db::open_memory().unwrap();
        import(&conn, no_root(), fixtures_dir(), false, false, None).unwrap();
        let unimported = scan_inbox(&conn, no_root(), &[fixtures_dir()]).unwrap();
        assert_eq!(unimported.len(), 0);
    }
}
