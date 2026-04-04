use rusqlite::Connection;

use crate::db::models::Track;
use crate::error::Result;

/// Check if a track with the given file path already exists in the database.
pub fn track_exists_by_path(conn: &Connection, path: &str) -> Result<bool> {
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM tracks WHERE file_path = ?1",
        [path],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get the track ID for a given file path, if it exists.
pub fn get_track_id_by_path(conn: &Connection, path: &str) -> Result<Option<i64>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM tracks WHERE file_path = ?1",
            [path],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Insert a track into the database. Returns the new track ID.
pub fn insert_track(
    conn: &Connection,
    track: &Track,
    album_id: Option<i64>,
    file_size: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO tracks (
            album_id, title, artist, track_number, disc_number,
            duration_ms, file_path, file_size, file_format,
            bitrate, sample_rate, source_dir, tag_status
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            album_id,
            track.title,
            track.artist,
            track.track_number,
            track.disc_number,
            track.duration_ms.map(|d| d as i64),
            track.file_path.display().to_string(),
            file_size,
            track.file_format.as_str(),
            track.bitrate,
            track.sample_rate,
            track.source_dir.as_ref().map(|p| p.display().to_string()),
            track.tag_status.as_str(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get or create an album. Returns (album_id, created).
pub fn get_or_create_album(
    conn: &Connection,
    title: &str,
    album_artist: Option<&str>,
    year: Option<i32>,
    genre: Option<&str>,
    track_total: u32,
) -> Result<(i64, bool)> {
    // Match by title + album_artist (both must match, including NULL)
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM albums WHERE title = ?1 AND album_artist IS ?2",
            rusqlite::params![title, album_artist],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok((id, false));
    }

    conn.execute(
        "INSERT INTO albums (title, album_artist, year, genre, track_total)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![title, album_artist, year, genre, track_total],
    )?;
    Ok((conn.last_insert_rowid(), true))
}

/// Get or create a collection by name. Returns (collection_id, created).
pub fn get_or_create_collection(conn: &Connection, name: &str) -> Result<(i64, bool)> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM collections WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok((id, false));
    }

    conn.execute("INSERT INTO collections (name) VALUES (?1)", [name])?;
    Ok((conn.last_insert_rowid(), true))
}

/// Add a track to a collection. Returns true if the track was newly added.
pub fn add_track_to_collection(conn: &Connection, collection_id: i64, track_id: i64) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO collection_tracks (collection_id, track_id) VALUES (?1, ?2)",
        rusqlite::params![collection_id, track_id],
    )?;
    Ok(conn.changes() > 0)
}

/// Count total tracks in the database.
pub fn count_tracks(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))?;
    Ok(count)
}

/// Count total albums in the database.
pub fn count_albums(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM albums", [], |row| row.get(0))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::models::{AudioFormat, TagStatus};
    use std::path::PathBuf;

    fn test_track() -> Track {
        Track {
            id: None,
            album_id: None,
            title: "Test Song".to_string(),
            artist: Some("Test Artist".to_string()),
            track_number: Some(1),
            disc_number: 1,
            duration_ms: Some(180000),
            mbid: None,
            file_path: PathBuf::from("/test/song.mp3"),
            file_format: AudioFormat::Mp3,
            bitrate: Some(320),
            sample_rate: Some(44100),
            tag_status: TagStatus::Unmatched,
            source_dir: Some(PathBuf::from("/test")),
        }
    }

    #[test]
    fn test_track_exists_by_path_empty_db() {
        let conn = db::open_memory().unwrap();
        assert!(!track_exists_by_path(&conn, "/some/path.mp3").unwrap());
    }

    #[test]
    fn test_insert_and_find_track() {
        let conn = db::open_memory().unwrap();
        let track = test_track();
        let id = insert_track(&conn, &track, None, Some(5000)).unwrap();
        assert!(id > 0);
        assert!(track_exists_by_path(&conn, "/test/song.mp3").unwrap());
    }

    #[test]
    fn test_insert_album_and_track() {
        let conn = db::open_memory().unwrap();
        let (album_id, created) =
            get_or_create_album(&conn, "Test Album", Some("Artist"), Some(2024), None, 10).unwrap();
        assert!(created);
        let track = test_track();
        let track_id = insert_track(&conn, &track, Some(album_id), None).unwrap();
        assert!(album_id > 0);

        // Same album again — should return existing
        let (album_id2, created2) =
            get_or_create_album(&conn, "Test Album", Some("Artist"), Some(2024), None, 10).unwrap();
        assert!(!created2);
        assert_eq!(album_id, album_id2);
        assert!(track_id > 0);

        let count = count_albums(&conn).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_collection_create_and_add() {
        let conn = db::open_memory().unwrap();
        let (coll_id, created) = get_or_create_collection(&conn, "My Playlist").unwrap();
        assert!(coll_id > 0);
        assert!(created);

        // Get again — should return same ID, not created
        let (coll_id2, created2) = get_or_create_collection(&conn, "My Playlist").unwrap();
        assert_eq!(coll_id, coll_id2);
        assert!(!created2);

        // Add a track
        let track = test_track();
        let track_id = insert_track(&conn, &track, None, None).unwrap();
        add_track_to_collection(&conn, coll_id, track_id).unwrap();

        // Adding again should not error (OR IGNORE)
        add_track_to_collection(&conn, coll_id, track_id).unwrap();
    }

    #[test]
    fn test_count_tracks() {
        let conn = db::open_memory().unwrap();
        assert_eq!(count_tracks(&conn).unwrap(), 0);

        let track = test_track();
        insert_track(&conn, &track, None, None).unwrap();
        assert_eq!(count_tracks(&conn).unwrap(), 1);
    }
}
