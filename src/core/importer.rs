use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use walkdir::WalkDir;

use crate::core::tagger;
use crate::db::models::{AudioFormat, SUPPORTED_EXTENSIONS, Track};
use crate::db::queries;
use crate::error::Result;

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

/// Import audio files from a path into the database.
///
/// - Scans for audio files
/// - Reads tags
/// - Groups into albums (unless `loose`)
/// - Inserts into SQLite
/// - Optionally adds to a collection
pub fn import(
    conn: &Connection,
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
        println!("No audio files found in {}", path.display());
        return Ok(result);
    }

    println!("Found {} audio file(s)", files.len());

    // Read tags for all files
    let mut tracks: Vec<Track> = Vec::new();
    let mut tag_data_map: Vec<Option<tagger::TagData>> = Vec::new();
    let mut duplicate_track_ids: Vec<(i64, String)> = Vec::new();

    for file_path in &files {
        let abs_path = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone());
        let path_str = abs_path.display().to_string();

        // Check for duplicates
        if queries::track_exists_by_path(conn, &path_str)? {
            if collection.is_some() {
                if let Some(track_id) = queries::get_track_id_by_path(conn, &path_str)? {
                    duplicate_track_ids.push((track_id, file_path.display().to_string()));
                }
            } else {
                eprintln!("  skip (already imported): {}", file_path.display());
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
                eprintln!("  error reading {}: {}", file_path.display(), e);
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
                eprintln!("  added to collection: {}", path);
                result.added_to_collection += 1;
            } else {
                eprintln!("  skip (already in collection): {}", path);
            }
        }
    }

    if tracks.is_empty() {
        if result.added_to_collection == 0 {
            println!("No new tracks to import.");
        }
        return Ok(result);
    }

    // Group into albums
    let groups = group_into_albums(&tracks, loose);

    if pretend {
        println!("\nDry run — would import {} track(s):", tracks.len());
        for (key, indices) in &groups {
            if loose {
                for &i in indices {
                    println!("  [loose] {}", tracks[i].title);
                }
            } else {
                let album_name = tag_data_map
                    .get(indices[0])
                    .and_then(|td| td.as_ref())
                    .and_then(|td| td.album.as_deref())
                    .unwrap_or(key);
                println!("  Album: {} ({} tracks)", album_name, indices.len());
                for &i in indices {
                    println!("    - {}", tracks[i].title);
                }
            }
        }
        return Ok(result);
    }

    // Insert into database
    let tx = conn.unchecked_transaction()?;

    for (key, indices) in &groups {
        // Create album if not loose and we have album info
        let album_id = if !loose {
            let first_tag = tag_data_map.get(indices[0]).and_then(|td| td.as_ref());

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

        for &i in indices {
            let track = &tracks[i];
            let file_size = std::fs::metadata(&track.file_path)
                .map(|m| m.len() as i64)
                .ok();

            let track_id = queries::insert_track(&tx, track, album_id, file_size)?;
            result.imported += 1;

            // Add to collection if requested
            if let Some(coll_id) = collection_id {
                queries::add_track_to_collection(&tx, coll_id, track_id)?;
            }

            println!("  imported: {}", track.title);
        }
    }

    tx.commit()?;

    Ok(result)
}

/// Scan inbox directories for unimported audio files.
/// Returns a list of paths that are not yet in the database.
pub fn scan_inbox(
    conn: &Connection,
    inbox_dirs: &[std::path::PathBuf],
) -> Result<Vec<std::path::PathBuf>> {
    let mut unimported = Vec::new();

    for dir in inbox_dirs {
        if !dir.exists() {
            eprintln!("  inbox dir not found: {}", dir.display());
            continue;
        }

        let files = scan_audio_files(dir);
        for file_path in files {
            let abs_path = std::fs::canonicalize(&file_path).unwrap_or(file_path);
            let path_str = abs_path.display().to_string();
            if !queries::track_exists_by_path(conn, &path_str)? {
                unimported.push(abs_path);
            }
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
        let result = import(&conn, fixtures_dir(), false, false, None).unwrap();
        assert!(result.imported >= 3);
        assert_eq!(result.skipped_duplicate, 0);
    }

    #[test]
    fn test_import_duplicates_skipped() {
        let conn = db::open_memory().unwrap();
        // Import once
        import(&conn, fixtures_dir(), false, false, None).unwrap();
        // Import again — should skip all
        let result = import(&conn, fixtures_dir(), false, false, None).unwrap();
        assert_eq!(result.imported, 0);
        assert!(result.skipped_duplicate >= 3);
    }

    #[test]
    fn test_import_loose() {
        let conn = db::open_memory().unwrap();
        let result = import(&conn, fixtures_dir(), true, false, None).unwrap();
        assert!(result.imported >= 3);
        assert_eq!(result.albums_created, 0);
    }

    #[test]
    fn test_import_pretend() {
        let conn = db::open_memory().unwrap();
        let result = import(&conn, fixtures_dir(), false, true, None).unwrap();
        assert_eq!(result.imported, 0); // pretend doesn't insert
    }

    #[test]
    fn test_import_with_collection() {
        let conn = db::open_memory().unwrap();
        let result = import(&conn, fixtures_dir(), true, false, Some("Test Collection")).unwrap();
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
        let result1 = import(&conn, fixtures_dir(), true, false, None).unwrap();
        assert!(result1.imported >= 3);

        // Re-import with collection — duplicates should be added to collection
        let result2 = import(&conn, fixtures_dir(), true, false, Some("Favorites")).unwrap();
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
    fn test_scan_inbox_empty() {
        let conn = db::open_memory().unwrap();
        let unimported = scan_inbox(&conn, &[fixtures_dir()]).unwrap();
        assert!(unimported.len() >= 3);
    }

    #[test]
    fn test_scan_inbox_after_import() {
        let conn = db::open_memory().unwrap();
        import(&conn, fixtures_dir(), false, false, None).unwrap();
        let unimported = scan_inbox(&conn, &[fixtures_dir()]).unwrap();
        assert_eq!(unimported.len(), 0);
    }
}
