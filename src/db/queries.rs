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

/// Set the cover-art path for an album. Always overwrites any existing
/// value. `path` is the absolute file path. Pass an empty string to clear.
pub fn set_album_cover_path(conn: &Connection, album_id: i64, path: &str) -> Result<()> {
    let value: Option<&str> = if path.is_empty() { None } else { Some(path) };
    conn.execute(
        "UPDATE albums SET cover_art_path = ?1 WHERE id = ?2",
        rusqlite::params![value, album_id],
    )?;
    Ok(())
}

/// Get the cover-art path for an album, or `None` if unset / album missing.
#[allow(dead_code)] // used by TUI cover preview in PR 2
pub fn get_album_cover_path(conn: &Connection, album_id: i64) -> Result<Option<String>> {
    let path: Option<Option<String>> = conn
        .query_row(
            "SELECT cover_art_path FROM albums WHERE id = ?1",
            [album_id],
            |row| row.get(0),
        )
        .ok();
    Ok(path.flatten())
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
#[allow(dead_code)] // used by tests; may be reused by future `stats` CLI
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
    pub mbid: Option<String>,
    pub label: Option<String>,
    pub genre: Option<String>,
    /// Absolute path to a cover image file on disk, or `None` if the album
    /// has no art tracked. Populated during import (sibling-file detection)
    /// or by the TUI Cover Art Archive fetch.
    pub cover_art_path: Option<String>,
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
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms,
                a.mbid, a.label, a.genre, a.cover_art_path
         FROM albums a
         LEFT JOIN tracks t ON t.album_id = a.id
         GROUP BY a.id
         ORDER BY {} {} NULLS LAST
         LIMIT ?1 OFFSET ?2",
        sort.order_clause(),
        dir,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], map_album_row)?;
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

/// Search albums matching the query against album-level fields only
/// (album title + album artist). Track-level fields are deliberately
/// excluded: a search for "花" should not surface an album just because
/// one of its tracks happens to be titled "靴の花火" — tracks are
/// returned separately via `search_tracks`, so folding them into album
/// results duplicates the hit.
///
/// Multi-word queries are split on whitespace; each term must appear in
/// at least one of the two fields (terms AND-combined, fields OR'd).
pub fn search_albums(conn: &Connection, query: &str, limit: usize) -> Result<Vec<AlbumRow>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| format!("%{}%", w))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // Build one "(a.title LIKE ?n OR a.album_artist LIKE ?n)" clause per term.
    let clauses: Vec<String> = (1..=terms.len())
        .map(|i| format!("(a.title LIKE ?{i} OR a.album_artist LIKE ?{i})"))
        .collect();
    let where_clause = clauses.join(" AND ");
    let limit_param = terms.len() + 1;

    let sql = format!(
        "SELECT a.id, a.title, a.album_artist, a.year,
                COUNT(t.id) as track_count,
                GROUP_CONCAT(DISTINCT t.file_format) as formats,
                COALESCE(SUM(t.duration_ms), 0) as total_duration_ms,
                a.mbid, a.label, a.genre, a.cover_art_path
         FROM albums a
         LEFT JOIN tracks t ON t.album_id = a.id
         WHERE {where_clause}
         GROUP BY a.id
         ORDER BY a.title COLLATE NOCASE
         LIMIT ?{limit_param}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::ToSql>> = terms
        .iter()
        .map(|t| Box::new(t.clone()) as Box<dyn rusqlite::ToSql>)
        .chain(std::iter::once(
            Box::new(limit as i64) as Box<dyn rusqlite::ToSql>
        ))
        .collect();
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_album_row)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Search tracks using FTS5 or LIKE fallback. Returns up to `limit` results.
pub fn search_tracks(conn: &Connection, query: &str, limit: usize) -> Result<Vec<TrackRow>> {
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks_fts", [], |row| row.get(0))
        .unwrap_or(0);

    if fts_count > 0 {
        let fts_query = query
            .split_whitespace()
            .map(|w| format!("\"{}\"*", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut stmt = conn.prepare(
            "SELECT t.id, t.title, t.artist, t.track_number, t.disc_number, t.duration_ms,
                    t.tag_status, t.bitrate, t.file_format, t.file_path
             FROM tracks_fts fts
             JOIN tracks t ON t.id = fts.rowid
             WHERE tracks_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], map_track_row)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    } else {
        let pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            "SELECT id, title, artist, track_number, disc_number, duration_ms,
                    tag_status, bitrate, file_format, file_path
             FROM tracks
             WHERE title LIKE ?1 OR artist LIKE ?1
             ORDER BY title COLLATE NOCASE
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], map_track_row)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }
}

/// Get an album by ID.
pub fn get_album(conn: &Connection, album_id: i64) -> Result<Option<AlbumRow>> {
    let row = conn
        .query_row(
            "SELECT a.id, a.title, a.album_artist, a.year,
                    COUNT(t.id) as track_count,
                    GROUP_CONCAT(DISTINCT t.file_format) as formats,
                    COALESCE(SUM(t.duration_ms), 0) as total_duration_ms,
                    a.mbid, a.label, a.genre, a.cover_art_path
             FROM albums a
             LEFT JOIN tracks t ON t.album_id = a.id
             WHERE a.id = ?1
             GROUP BY a.id",
            [album_id],
            map_album_row,
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

/// Load every `collection_file_path` for a collection, keyed by track_id.
/// Used by the TUI collection detail view to show where each track
/// physically lives (organized collection copy vs the track's main path).
pub fn get_collection_file_paths(
    conn: &Connection,
    collection_id: i64,
) -> Result<std::collections::HashMap<i64, String>> {
    let mut stmt = conn.prepare(
        "SELECT track_id, collection_file_path FROM collection_tracks
         WHERE collection_id = ?1 AND collection_file_path IS NOT NULL",
    )?;
    let rows = stmt.query_map([collection_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (id, path) = row?;
        map.insert(id, path);
    }
    Ok(map)
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

/// Rename a collection.
pub fn rename_collection(conn: &Connection, collection_id: i64, new_name: &str) -> Result<()> {
    conn.execute(
        "UPDATE collections SET name = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![new_name, collection_id],
    )?;
    Ok(())
}

/// Find a collection by name (case-insensitive exact match).
pub fn find_collection_by_name(conn: &Connection, name: &str) -> Result<Option<i64>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM collections WHERE name = ?1 COLLATE NOCASE",
            [name],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Track in a collection with information about its other "homes".
/// Used by collection deletion to decide which tracks would be orphaned.
#[derive(Debug, Clone)]
pub struct CollectionTrackHomes {
    pub track_id: i64,
    pub track_file_path: String,
    pub collection_file_path: Option<String>,
    pub has_album: bool,
    pub other_collection_count: u32,
}

/// For each track in a collection, return its file paths and how many other
/// "homes" it has (an album, or other collections-with-files).
pub fn get_collection_tracks_with_other_homes(
    conn: &Connection,
    collection_id: i64,
) -> Result<Vec<CollectionTrackHomes>> {
    let mut stmt = conn.prepare(
        "SELECT
            t.id,
            t.file_path,
            ct.collection_file_path,
            (t.album_id IS NOT NULL) as has_album,
            (SELECT COUNT(*) FROM collection_tracks ct2
             WHERE ct2.track_id = t.id AND ct2.collection_id != ?1
                AND ct2.collection_file_path IS NOT NULL) as other_collection_count
         FROM collection_tracks ct
         JOIN tracks t ON t.id = ct.track_id
         WHERE ct.collection_id = ?1",
    )?;
    let rows = stmt.query_map([collection_id], |row| {
        Ok(CollectionTrackHomes {
            track_id: row.get(0)?,
            track_file_path: row.get(1)?,
            collection_file_path: row.get(2)?,
            has_album: row.get::<_, i64>(3)? != 0,
            other_collection_count: row.get::<_, i64>(4)? as u32,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Delete a track entirely from the database (cascades collection_tracks).
pub fn delete_track(conn: &Connection, track_id: i64) -> Result<()> {
    conn.execute("DELETE FROM tracks WHERE id = ?1", [track_id])?;
    Ok(())
}

/// Delete an album row. Does NOT delete its tracks — caller is responsible for
/// handling them first. The `album_id` FK on `tracks` enforces "no action", so
/// with `PRAGMA foreign_keys=ON` this call will fail if any track still
/// references the album.
pub fn delete_album(conn: &Connection, album_id: i64) -> Result<()> {
    conn.execute("DELETE FROM albums WHERE id = ?1", [album_id])?;
    Ok(())
}

/// Info about a track needed when computing a delete plan — the primary file
/// path, album (if any), and every collection-copy path the track owns.
#[derive(Debug, Clone)]
pub struct TrackDeleteInfo {
    pub track_id: i64,
    pub title: String,
    pub file_path: String,
    pub album_id: Option<i64>,
    /// Every `collection_tracks.collection_file_path` this track has set.
    pub collection_copies: Vec<String>,
}

/// Load delete-planning info for a set of track ids.
pub fn get_tracks_delete_info(
    conn: &Connection,
    track_ids: &[i64],
) -> Result<Vec<TrackDeleteInfo>> {
    if track_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = track_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, title, file_path, album_id FROM tracks WHERE id IN ({})",
        placeholders
    );
    let params: Vec<&dyn rusqlite::ToSql> =
        track_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(TrackDeleteInfo {
            track_id: row.get(0)?,
            title: row.get(1)?,
            file_path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            album_id: row.get(3)?,
            collection_copies: Vec::new(),
        })
    })?;
    let mut result: Vec<TrackDeleteInfo> = Vec::new();
    for r in rows {
        result.push(r?);
    }

    let mut cstmt = conn.prepare(
        "SELECT collection_file_path FROM collection_tracks
         WHERE track_id = ?1 AND collection_file_path IS NOT NULL",
    )?;
    for info in &mut result {
        let copies = cstmt.query_map([info.track_id], |row| row.get::<_, String>(0))?;
        for c in copies {
            info.collection_copies.push(c?);
        }
    }
    Ok(result)
}

/// List track ids belonging to a set of albums.
pub fn list_tracks_for_albums(conn: &Connection, album_ids: &[i64]) -> Result<Vec<i64>> {
    if album_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = album_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id FROM tracks WHERE album_id IN ({})",
        placeholders
    );
    let params: Vec<&dyn rusqlite::ToSql> =
        album_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Short display label for an album: (artist, title).
pub fn get_album_label(conn: &Connection, album_id: i64) -> Result<Option<(String, String)>> {
    let row = conn
        .query_row(
            "SELECT COALESCE(album_artist, '(unknown)'), title FROM albums WHERE id = ?1",
            [album_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    Ok(row)
}

/// Update a track with MusicBrainz metadata.
pub fn update_track_mb(
    conn: &Connection,
    track_id: i64,
    mbid: &str,
    artist: &str,
    title: &str,
    tag_status: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET mbid = ?1, artist = ?2, title = ?3, tag_status = ?4,
                modified_date = datetime('now')
         WHERE id = ?5",
        rusqlite::params![mbid, artist, title, tag_status, track_id],
    )?;
    Ok(())
}

/// Update an album with MusicBrainz metadata.
pub fn update_album_mb(
    conn: &Connection,
    album_id: i64,
    mbid: &str,
    release_mbid: Option<&str>,
    artist: &str,
    title: &str,
    year: Option<i32>,
    label: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE albums SET mbid = ?1, release_mbid = ?2, album_artist = ?3,
                title = ?4, year = ?5, label = ?6, updated_at = datetime('now')
         WHERE id = ?7",
        rusqlite::params![mbid, release_mbid, artist, title, year, label, album_id],
    )?;
    Ok(())
}

/// Set a track's tag_status field.
pub fn set_track_tag_status(conn: &Connection, track_id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET tag_status = ?1, modified_date = datetime('now') WHERE id = ?2",
        rusqlite::params![status, track_id],
    )?;
    Ok(())
}

// ── Organize queries ────────────────────────────────────────────────

/// Track data needed to compute an organize target path.
#[derive(Debug, Clone)]
pub struct OrganizeTrackRow {
    pub id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: u32,
    pub file_path: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub disc_total: Option<u32>,
    /// (collection_id, collection_name, collection path_template or None)
    pub collections: Vec<(i64, String, Option<String>)>,
}

/// Get all tracks with album + collection info for organizing.
pub fn get_all_tracks_for_organize(
    conn: &Connection,
    filter: &crate::core::organizer::OrganizeFilter,
) -> Result<Vec<OrganizeTrackRow>> {
    use crate::core::organizer::OrganizeFilter;

    let (where_clause, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match filter {
        OrganizeFilter::All => ("1=1".to_string(), vec![]),
        OrganizeFilter::Artist(a) => (
            "(t.artist = ?1 OR a.album_artist = ?1)".to_string(),
            vec![Box::new(a.clone())],
        ),
        OrganizeFilter::Album(name) => (
            "a.title = ?1".to_string(),
            vec![Box::new(name.clone())],
        ),
        OrganizeFilter::AlbumId(id) => (
            "t.album_id = ?1".to_string(),
            vec![Box::new(*id)],
        ),
        OrganizeFilter::Loose => ("t.album_id IS NULL".to_string(), vec![]),
        OrganizeFilter::Path(p) => (
            "t.file_path LIKE ?1 || '%'".to_string(),
            vec![Box::new(p.display().to_string())],
        ),
        OrganizeFilter::Collection(name) => (
            "EXISTS (SELECT 1 FROM collection_tracks ct2 JOIN collections c2 ON c2.id = ct2.collection_id WHERE ct2.track_id = t.id AND c2.name = ?1)".to_string(),
            vec![Box::new(name.clone())],
        ),
    };

    let sql = format!(
        "SELECT t.id, t.title, t.artist, t.track_number, t.disc_number, t.file_path,
                t.album_id,
                a.title, a.album_artist, a.year, a.genre, a.label, a.disc_total
         FROM tracks t
         LEFT JOIN albums a ON t.album_id = a.id
         WHERE {}
         ORDER BY a.album_artist, a.title, t.disc_number, t.track_number",
        where_clause
    );

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(OrganizeTrackRow {
            id: row.get(0)?,
            title: row.get(1)?,
            artist: row.get(2)?,
            track_number: row.get::<_, Option<u32>>(3)?,
            disc_number: row.get::<_, Option<u32>>(4)?.unwrap_or(1),
            file_path: row.get(5)?,
            album_id: row.get(6)?,
            album_title: row.get(7)?,
            album_artist: row.get(8)?,
            year: row.get(9)?,
            genre: row.get(10)?,
            label: row.get(11)?,
            disc_total: row.get(12)?,
            collections: Vec::new(), // Filled below
        })
    })?;

    let mut tracks: Vec<OrganizeTrackRow> = Vec::new();
    for row in rows {
        tracks.push(row?);
    }

    // Load collection memberships
    let mut coll_stmt = conn.prepare(
        "SELECT ct.collection_id, c.name, c.path_template
         FROM collection_tracks ct
         JOIN collections c ON c.id = ct.collection_id
         WHERE ct.track_id = ?1",
    )?;
    for track in &mut tracks {
        let colls = coll_stmt.query_map([track.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for c in colls {
            track.collections.push(c?);
        }
    }

    Ok(tracks)
}

/// Update a track's file_path in the database.
/// List every (track_id, file_path) currently in the library.
/// Used by the organizer for collision detection.
pub fn list_all_track_paths(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, file_path FROM tracks")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn update_track_path(conn: &Connection, track_id: i64, new_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET file_path = ?1, modified_date = datetime('now') WHERE id = ?2",
        rusqlite::params![new_path, track_id],
    )?;
    Ok(())
}

/// Update a collection track's file path.
pub fn update_collection_track_path(
    conn: &Connection,
    collection_id: i64,
    track_id: i64,
    path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE collection_tracks SET collection_file_path = ?1
         WHERE collection_id = ?2 AND track_id = ?3",
        rusqlite::params![path, collection_id, track_id],
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

fn map_album_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: row.get(0)?,
        title: row.get(1)?,
        album_artist: row.get(2)?,
        year: row.get(3)?,
        track_count: row.get(4)?,
        formats: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        total_duration_ms: row.get(6)?,
        mbid: row.get(7)?,
        label: row.get(8)?,
        genre: row.get(9)?,
        cover_art_path: row.get(10)?,
    })
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
