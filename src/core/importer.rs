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
    pub skipped_non_utf8: u32,
    pub added_to_collection: u32,
    pub albums_created: u32,
    pub albums_existing: u32,
    pub collection_created: bool,
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct ScanAudioResult {
    pub files: Vec<std::path::PathBuf>,
    pub skipped_non_utf8: u32,
}

/// Scan a directory for audio files, returning absolute paths plus skipped-file counts.
pub fn scan_audio_files_with_report(path: impl AsRef<Path>) -> ScanAudioResult {
    let path = path.as_ref();
    let mut result = ScanAudioResult::default();

    for entry in WalkDir::new(path).follow_links(true).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.to_str().is_none() {
            tracing::warn!("skipping non-UTF-8 path: {}", path.display());
            result.skipped_non_utf8 += 1;
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.contains(tagger::TMP_MARKER))
        {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            result.files.push(entry.into_path());
        }
    }

    result.files.sort();
    result
}

/// Stable grouping key for album imports. Non-loose imports group by source
/// directory so a directory of hand-picked loose tracks can be reviewed and
/// assigned to a collection as one unit. Album creation is guarded later by
/// tag consistency, so mixed folders do not stamp every track with the first
/// track's album.
pub(crate) fn album_group_key(track: &Track, index: usize, loose: bool) -> String {
    if loose {
        return format!("__loose__{}", index);
    }

    track
        .source_dir
        .as_deref()
        .or_else(|| track.file_path.parent())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

/// Group tracks by source directory.
/// Returns a map of group key -> list of tracks.
/// If `loose` is true, each track is its own group (no album grouping).
fn group_into_albums(tracks: &[Track], loose: bool) -> HashMap<String, Vec<usize>> {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, track) in tracks.iter().enumerate() {
        let key = album_group_key(track, i, loose);
        groups.entry(key).or_default().push(i);
    }

    groups
}

fn group_has_consistent_album_tags(
    indices: &[usize],
    tag_data_map: &[Option<tagger::TagData>],
) -> bool {
    let key = |idx: usize| -> Option<(String, Option<String>)> {
        tag_data_map
            .get(idx)
            .and_then(|td| td.as_ref())
            .and_then(|td| {
                td.album.as_ref().map(|album| {
                    (
                        album.trim().to_lowercase(),
                        td.album_artist
                            .as_deref()
                            .or(td.artist.as_deref())
                            .map(|s| s.trim().to_lowercase()),
                    )
                })
            })
    };

    let first_key = indices.first().and_then(|&idx| key(idx));
    first_key.is_some() && indices.iter().all(|&idx| key(idx) == first_key)
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
        if path.to_str().is_none() {
            tracing::warn!("skipping non-UTF-8 path: {}", path.display());
            result.skipped_non_utf8 += 1;
            Vec::new()
        } else {
            vec![path.to_path_buf()]
        }
    } else {
        let scan = scan_audio_files_with_report(path);
        result.skipped_non_utf8 = scan.skipped_non_utf8;
        scan.files
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

        match tagger::read_track_with_tags(&abs_path) {
            Ok((mut track, tag_data)) => {
                track.file_path = abs_path;
                tag_data_map.push(Some(tag_data));
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

    // Group into import batches.
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

        // Create album only when this batch has coherent album tags. Mixed
        // source-directory groups are imported as loose tracks (and can still
        // be assigned to a collection as one unit).
        let album_id = if !loose && group_has_consistent_album_tags(indices, &tag_data_map) {
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
#[derive(Debug, Default)]
pub struct ScanInboxResult {
    pub files: Vec<std::path::PathBuf>,
    pub skipped_non_utf8: u32,
}

pub fn scan_inbox_with_report(
    conn: &Connection,
    music_dir: &Path,
    inbox_dirs: &[std::path::PathBuf],
) -> Result<ScanInboxResult> {
    // Build the exclusion set once per call — O(N) memory in library size,
    // but a single pass per table beats a per-file query.
    let known = queries::list_all_known_paths(conn, music_dir)?;
    let mut result = ScanInboxResult::default();

    for dir in inbox_dirs {
        if !dir.exists() {
            tracing::warn!("inbox dir not found: {}", dir.display());
            continue;
        }

        let scan = scan_audio_files_with_report(dir);
        result.skipped_non_utf8 += scan.skipped_non_utf8;
        for file_path in scan.files {
            let abs_path = std::fs::canonicalize(&file_path).unwrap_or(file_path);
            let path_str = abs_path.display().to_string();
            if known.contains(&path_str) {
                continue;
            }
            result.files.push(abs_path);
        }
    }

    Ok(result)
}

pub fn scan_inbox(
    conn: &Connection,
    music_dir: &Path,
    inbox_dirs: &[std::path::PathBuf],
) -> Result<Vec<std::path::PathBuf>> {
    scan_inbox_with_report(conn, music_dir, inbox_dirs).map(|r| r.files)
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
        let files = scan_audio_files_with_report(fixtures_dir()).files;
        // We have tagged.mp3, no_title.mp3, cjk_tagged.mp3 (not_audio.txt is excluded)
        assert!(files.len() >= 3);
        assert!(files.iter().all(|f| {
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str())
        }));
    }

    fn test_track(path: PathBuf, source_dir: PathBuf, title: &str) -> Track {
        Track {
            id: None,
            album_id: None,
            title: title.to_string(),
            artist: Some("Artist".to_string()),
            track_number: Some(1),
            disc_number: 1,
            duration_ms: None,
            mbid: None,
            file_path: path,
            file_format: crate::db::models::AudioFormat::Mp3,
            bitrate: None,
            sample_rate: None,
            tag_status: crate::db::models::TagStatus::Unmatched,
            source_dir: Some(source_dir),
        }
    }

    fn tag_with_album(album: &str) -> tagger::TagData {
        tagger::TagData {
            title: None,
            artist: Some("Artist".to_string()),
            album: Some(album.to_string()),
            album_artist: Some("Artist".to_string()),
            year: None,
            track_number: Some(1),
            disc_number: Some(1),
            genre: None,
            duration: None,
            mb_release_id: None,
        }
    }

    #[test]
    fn group_into_albums_keeps_mixed_album_tags_in_one_directory_together() {
        let dir = PathBuf::from("/inbox/mixed");
        let tracks = vec![
            test_track(dir.join("a.mp3"), dir.clone(), "A"),
            test_track(dir.join("b.mp3"), dir.clone(), "B"),
            test_track(dir.join("c.mp3"), dir.clone(), "C"),
        ];
        let tags = vec![
            Some(tag_with_album("Album A")),
            Some(tag_with_album("album a ")),
            Some(tag_with_album("Album B")),
        ];

        let groups = group_into_albums(&tracks, false);

        assert_eq!(groups.len(), 1);
        assert!(groups.values().any(|indices| indices == &vec![0, 1, 2]));
        assert!(!group_has_consistent_album_tags(&[0, 1, 2], &tags));
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

    #[cfg(unix)]
    #[test]
    fn scan_audio_files_skips_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;

        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join(std::ffi::OsStr::from_bytes(b"bad\xFF.mp3"));
        if let Err(e) = std::fs::write(&bad, b"") {
            // Some Unix filesystems / platform layers (notably macOS/APFS)
            // reject arbitrary non-UTF-8 byte sequences before our scanner
            // can see them. The scanner behavior is still exercised on
            // filesystems that allow such names; elsewhere this test is not
            // applicable.
            eprintln!("skipping non-UTF-8 path test: filesystem rejected filename: {e}");
            return;
        }

        let result = scan_audio_files_with_report(tmp.path());
        assert!(result.files.is_empty());
        assert_eq!(result.skipped_non_utf8, 1);
    }

    #[test]
    fn scan_audio_files_skips_kyoku_tmp_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("real.mp3"), b"").unwrap();
        std::fs::write(tmp.path().join("real.kyoku-tmp.mp3"), b"").unwrap();

        let files = scan_audio_files_with_report(tmp.path()).files;

        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].file_name().and_then(|s| s.to_str()),
            Some("real.mp3")
        );
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
