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

// ── TUI query types ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub album_artist: Option<String>,
    pub year: Option<i32>,
    pub track_count: i64,
    pub formats: String,
    pub total_duration_ms: i64,
}

#[derive(Debug, Clone)]
pub struct TrackRow {
    pub id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: u32,
    pub duration_ms: Option<u64>,
    pub tag_status: String,
    pub bitrate: Option<u32>,
    pub file_format: String,
    pub file_path: String,
}

#[derive(Debug, Clone)]
pub struct CollectionRow {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub track_count: i64,
    pub total_duration_ms: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum AlbumSort {
    Artist,
    Album,
    Year,
    TrackCount,
}

impl AlbumSort {
    pub fn next(self) -> Self {
        match self {
            Self::Artist => Self::Album,
            Self::Album => Self::Year,
            Self::Year => Self::TrackCount,
            Self::TrackCount => Self::Artist,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Year => "year",
            Self::TrackCount => "tracks",
        }
    }

    fn order_clause(self) -> &'static str {
        match self {
            Self::Artist => "a.album_artist COLLATE NOCASE",
            Self::Album => "a.title COLLATE NOCASE",
            Self::Year => "a.year",
            Self::TrackCount => "track_count",
        }
    }
}

/// List albums with pagination and sorting.
pub fn list_albums(
    conn: &Connection,
    sort: AlbumSort,
    ascending: bool,
    offset: usize,
    limit: usize,
) -> Result<Vec<AlbumRow>> {
    let dir = if ascending { "ASC" } else { "DESC" };
    let sql = format!(
        "SELECT a.id, a.title, a.album_artist, a.year,
                COUNT(t.id) as track_count,
                GROUP_CONCAT(DISTINCT t.file_format) as formats,
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
         FROM albums a
         LEFT JOIN tracks t ON t.album_id = a.id
         GROUP BY a.id
         ORDER BY {} {} NULLS LAST
         LIMIT ?1 OFFSET ?2",
        sort.order_clause(),
        dir,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
        Ok(AlbumRow {
            id: row.get(0)?,
            title: row.get(1)?,
            album_artist: row.get(2)?,
            year: row.get(3)?,
            track_count: row.get(4)?,
            formats: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            total_duration_ms: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Count loose tracks (tracks with no album).
pub fn count_loose_tracks(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tracks WHERE album_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// List loose tracks with pagination.
pub fn list_loose_tracks(conn: &Connection, offset: usize, limit: usize) -> Result<Vec<TrackRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, artist, track_number, disc_number, duration_ms,
                tag_status, bitrate, file_format, file_path
         FROM tracks WHERE album_id IS NULL
         ORDER BY title COLLATE NOCASE
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], map_track_row)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Search albums using FTS5. Falls back to LIKE if FTS is empty.
pub fn search_albums(conn: &Connection, query: &str, limit: usize) -> Result<Vec<AlbumRow>> {
    // Try FTS5 first
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks_fts", [], |row| row.get(0))
        .unwrap_or(0);

    if fts_count > 0 {
        search_albums_fts(conn, query, limit)
    } else {
        search_albums_like(conn, query, limit)
    }
}

fn search_albums_fts(conn: &Connection, query: &str, limit: usize) -> Result<Vec<AlbumRow>> {
    // FTS5 query: match across title, artist, album_title
    let fts_query = query
        .split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");

    let mut stmt = conn.prepare(
        "SELECT DISTINCT a.id, a.title, a.album_artist, a.year,
                COUNT(t.id) as track_count,
                GROUP_CONCAT(DISTINCT t.file_format) as formats,
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
         FROM tracks_fts fts
         JOIN tracks t ON t.id = fts.rowid
         LEFT JOIN albums a ON t.album_id = a.id
         WHERE tracks_fts MATCH ?1
         GROUP BY a.id
         HAVING a.id IS NOT NULL
         ORDER BY a.title COLLATE NOCASE
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
        Ok(AlbumRow {
            id: row.get(0)?,
            title: row.get(1)?,
            album_artist: row.get(2)?,
            year: row.get(3)?,
            track_count: row.get(4)?,
            formats: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            total_duration_ms: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

fn search_albums_like(conn: &Connection, query: &str, limit: usize) -> Result<Vec<AlbumRow>> {
    let pattern = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT a.id, a.title, a.album_artist, a.year,
                COUNT(t.id) as track_count,
                GROUP_CONCAT(DISTINCT t.file_format) as formats,
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
         FROM albums a
         LEFT JOIN tracks t ON t.album_id = a.id
         WHERE a.title LIKE ?1 OR a.album_artist LIKE ?1
               OR t.title LIKE ?1 OR t.artist LIKE ?1
         GROUP BY a.id
         ORDER BY a.title COLLATE NOCASE
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
        Ok(AlbumRow {
            id: row.get(0)?,
            title: row.get(1)?,
            album_artist: row.get(2)?,
            year: row.get(3)?,
            track_count: row.get(4)?,
            formats: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            total_duration_ms: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Get an album by ID.
pub fn get_album(conn: &Connection, album_id: i64) -> Result<Option<AlbumRow>> {
    let row = conn
        .query_row(
            "SELECT a.id, a.title, a.album_artist, a.year,
                    COUNT(t.id) as track_count,
                    GROUP_CONCAT(DISTINCT t.file_format) as formats,
                    COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
             FROM albums a
             LEFT JOIN tracks t ON t.album_id = a.id
             WHERE a.id = ?1
             GROUP BY a.id",
            [album_id],
            |row| {
                Ok(AlbumRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    album_artist: row.get(2)?,
                    year: row.get(3)?,
                    track_count: row.get(4)?,
                    formats: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    total_duration_ms: row.get(6)?,
                })
            },
        )
        .ok();
    Ok(row)
}

/// Get tracks for an album, ordered by disc/track number.
pub fn get_album_tracks(conn: &Connection, album_id: i64) -> Result<Vec<TrackRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, artist, track_number, disc_number, duration_ms,
                tag_status, bitrate, file_format, file_path
         FROM tracks WHERE album_id = ?1
         ORDER BY disc_number, track_number, title COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([album_id], map_track_row)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// List all collections with track counts.
pub fn list_collections(conn: &Connection) -> Result<Vec<CollectionRow>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.description,
                COUNT(ct.track_id) as track_count,
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
         FROM collections c
         LEFT JOIN collection_tracks ct ON ct.collection_id = c.id
         LEFT JOIN tracks t ON t.id = ct.track_id
         GROUP BY c.id
         ORDER BY c.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(CollectionRow {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            track_count: row.get(3)?,
            total_duration_ms: row.get(4)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Search collections by name.
pub fn search_collections(conn: &Connection, query: &str) -> Result<Vec<CollectionRow>> {
    let pattern = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.description,
                COUNT(ct.track_id) as track_count,
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
         FROM collections c
         LEFT JOIN collection_tracks ct ON ct.collection_id = c.id
         LEFT JOIN tracks t ON t.id = ct.track_id
         WHERE c.name LIKE ?1
         GROUP BY c.id
         ORDER BY c.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([pattern], |row| {
        Ok(CollectionRow {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            track_count: row.get(3)?,
            total_duration_ms: row.get(4)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Get tracks in a collection.
pub fn get_collection_tracks(
    conn: &Connection,
    collection_id: i64,
    offset: usize,
    limit: usize,
) -> Result<Vec<TrackRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.artist, t.track_number, t.disc_number, t.duration_ms,
                t.tag_status, t.bitrate, t.file_format, t.file_path
         FROM collection_tracks ct
         JOIN tracks t ON t.id = ct.track_id
         WHERE ct.collection_id = ?1
         ORDER BY ct.position, t.title COLLATE NOCASE
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![collection_id, limit as i64, offset as i64],
        map_track_row,
    )?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Create a new collection.
pub fn create_collection(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute("INSERT INTO collections (name) VALUES (?1)", [name])?;
    Ok(conn.last_insert_rowid())
}

/// Delete a collection (cascade removes track associations).
pub fn delete_collection(conn: &Connection, collection_id: i64) -> Result<()> {
    conn.execute("DELETE FROM collections WHERE id = ?1", [collection_id])?;
    Ok(())
}

/// Remove a track from a collection.
pub fn remove_track_from_collection(
    conn: &Connection,
    collection_id: i64,
    track_id: i64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM collection_tracks WHERE collection_id = ?1 AND track_id = ?2",
        rusqlite::params![collection_id, track_id],
    )?;
    Ok(())
}

/// Get a single track by ID.
pub fn get_track(conn: &Connection, track_id: i64) -> Result<Option<TrackRow>> {
    let row = conn
        .query_row(
            "SELECT id, title, artist, track_number, disc_number, duration_ms,
                    tag_status, bitrate, file_format, file_path
             FROM tracks WHERE id = ?1",
            [track_id],
            map_track_row,
        )
        .ok();
    Ok(row)
}

/// Update a track's fields in the database.
pub fn update_track_fields(
    conn: &Connection,
    track_id: i64,
    fields: &[(&str, &str)],
) -> Result<()> {
    for (field, value) in fields {
        let allowed = ["title", "artist", "track_number", "disc_number"];
        if !allowed.contains(field) {
            continue;
        }
        let sql = format!("UPDATE tracks SET {} = ?1 WHERE id = ?2", field);
        conn.execute(&sql, rusqlite::params![value, track_id])?;
    }
    conn.execute(
        "UPDATE tracks SET modified_date = datetime('now') WHERE id = ?1",
        [track_id],
    )?;
    Ok(())
}

/// Rename an album (update its title).
pub fn rename_album(conn: &Connection, album_id: i64, new_title: &str) -> Result<()> {
    conn.execute(
        "UPDATE albums SET title = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![new_title, album_id],
    )?;
    Ok(())
}

/// Rebuild the FTS5 index from existing track data.
pub fn rebuild_fts_index(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM tracks_fts;
         INSERT INTO tracks_fts(rowid, title, artist, album_title)
         SELECT t.id, t.title, t.artist, a.title
         FROM tracks t LEFT JOIN albums a ON t.album_id = a.id;",
    )?;
    Ok(())
}

fn map_track_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackRow> {
    Ok(TrackRow {
        id: row.get(0)?,
        title: row.get(1)?,
        artist: row.get(2)?,
        track_number: row.get::<_, Option<u32>>(3)?,
        disc_number: row.get::<_, Option<u32>>(4)?.unwrap_or(1),
        duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        tag_status: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        bitrate: row.get::<_, Option<u32>>(7)?,
        file_format: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
        file_path: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
    })
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
