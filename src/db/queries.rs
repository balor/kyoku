use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::core::collection_order::{self, CollectionOrderItem};
use crate::core::paths;
use crate::db::models::Track;
use crate::error::Result;

/// Every file path the DB currently knows about — `tracks.file_path`, every
/// non-null `collection_tracks.collection_file_path`, and every
/// `orphaned_files.file_path`. Used by the import scanner to skip files that
/// are already accounted for somewhere, including collection copies sitting
/// inside `music_dir` and pending-orphan leftovers.
///
/// Stored paths are resolved against `music_dir` so the returned set is
/// fully absolute, ready to compare against scanner output. Each path is
/// also inserted in its canonicalized form (when canonicalize succeeds) so
/// symlink / NFC-vs-NFD variants don't slip past the dedup check.
pub fn list_all_known_paths(conn: &Connection, music_dir: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    let mut insert_both = |stored: String| {
        let abs = paths::from_db_path(&stored, music_dir);
        let abs_str = abs.display().to_string();
        if let Ok(canon) = std::fs::canonicalize(&abs) {
            out.insert(canon.display().to_string());
        }
        out.insert(abs_str);
    };
    // One statement per source table — UNIONing inside SQLite would also
    // work but this keeps the borrow of `conn` straightforward.
    let mut stmt = conn.prepare("SELECT file_path FROM tracks")?;
    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
        insert_both(row?);
    }
    let mut stmt = conn.prepare(
        "SELECT collection_file_path FROM collection_tracks \
         WHERE collection_file_path IS NOT NULL",
    )?;
    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
        insert_both(row?);
    }
    let mut stmt = conn.prepare("SELECT file_path FROM orphaned_files")?;
    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
        insert_both(row?);
    }
    Ok(out)
}

/// Check if a track with the given file path already exists in the database.
/// `path` is the caller's absolute path; we normalize to DB form before lookup.
pub fn track_exists_by_path(conn: &Connection, music_dir: &Path, path: &str) -> Result<bool> {
    let stored = paths::to_db_path(Path::new(path), music_dir);
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM tracks WHERE file_path = ?1",
        [&stored],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get the track ID for a given absolute file path, if it exists.
pub fn get_track_id_by_path(
    conn: &Connection,
    music_dir: &Path,
    path: &str,
) -> Result<Option<i64>> {
    let stored = paths::to_db_path(Path::new(path), music_dir);
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM tracks WHERE file_path = ?1",
            [&stored],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id)
}

/// Insert a track into the database. Returns the new track ID. The track's
/// `file_path` is normalised against `music_dir` (relative when under it).
/// `source_dir` is left untouched — it's a historical breadcrumb, not a path
/// the rest of the system follows back to a file.
pub fn insert_track(
    conn: &Connection,
    music_dir: &Path,
    track: &Track,
    album_id: Option<i64>,
    file_size: Option<i64>,
) -> Result<i64> {
    let file_path = paths::to_db_path(&track.file_path, music_dir);
    conn.execute(
        "INSERT INTO tracks (
            album_id, title, artist, track_number, disc_number,
            duration_ms, mbid, file_path, file_size, file_format,
            bitrate, sample_rate, source_dir, tag_status
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            album_id,
            track.title,
            track.artist,
            track.track_number,
            track.disc_number,
            track.duration_ms.map(|d| d as i64),
            track.mbid.as_deref(),
            file_path,
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
        .optional()?;

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
/// value. `path` is an absolute file path; it's normalised against
/// `music_dir` before storage. Pass an empty string to clear.
pub fn set_album_cover_path(
    conn: &Connection,
    music_dir: &Path,
    album_id: i64,
    path: &str,
) -> Result<()> {
    let stored = if path.is_empty() {
        None
    } else {
        Some(paths::to_db_path(Path::new(path), music_dir))
    };
    conn.execute(
        "UPDATE albums SET cover_art_path = ?1 WHERE id = ?2",
        rusqlite::params![stored, album_id],
    )?;
    Ok(())
}

/// Get the cover-art path for an album, resolved to an absolute filesystem
/// path. `None` when unset or the album is missing.
#[allow(dead_code)] // used by TUI cover preview in PR 2
pub fn get_album_cover_path(
    conn: &Connection,
    music_dir: &Path,
    album_id: i64,
) -> Result<Option<String>> {
    let path: Option<Option<String>> = conn
        .query_row(
            "SELECT cover_art_path FROM albums WHERE id = ?1",
            [album_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(path
        .flatten()
        .map(|s| paths::from_db_path(&s, music_dir).display().to_string()))
}

/// Get or create a collection by name. Returns (collection_id, created).
pub fn get_or_create_collection(conn: &Connection, name: &str) -> Result<(i64, bool)> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM collections WHERE name = ?1 COLLATE NOCASE",
            [name],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        return Ok((id, false));
    }

    conn.execute("INSERT INTO collections (name) VALUES (?1)", [name])?;
    Ok((conn.last_insert_rowid(), true))
}

/// Look up a collection id by exact name, case-insensitively. Unlike
/// `get_or_create_collection` this never creates anything — used by
/// `kyoku play --collection`, where a typo should be an error, not a new
/// empty collection.
pub fn find_collection_id_by_name(conn: &Connection, name: &str) -> Result<Option<(i64, String)>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, name FROM collections WHERE name = ?1 COLLATE NOCASE",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(row)
}

/// Find albums by exact title, case-insensitively. Several albums can
/// share a title (different artists, different pressings) — the caller
/// decides how to disambiguate. Returns (id, title, album_artist).
pub fn find_albums_by_title(
    conn: &Connection,
    title: &str,
) -> Result<Vec<(i64, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, album_artist FROM albums WHERE title = ?1 COLLATE NOCASE ORDER BY album_artist",
    )?;
    let rows = stmt.query_map([title], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Add a track to a collection. Returns true if the track was newly added.
///
/// New memberships get an append position. Existing memberships are left
/// untouched and do not consume a position.
pub fn add_track_to_collection(
    conn: &Connection,
    collection_id: i64,
    track_id: i64,
) -> Result<bool> {
    add_tracks_to_collection_ordered(conn, collection_id, &[track_id]).map(|n| n > 0)
}

/// Add tracks to a collection in the provided order. Returns the number of
/// newly inserted memberships. Existing memberships are skipped without
/// changing their stored position.
pub fn add_tracks_to_collection_ordered(
    conn: &Connection,
    collection_id: i64,
    track_ids: &[i64],
) -> Result<u32> {
    let mut next_position = next_collection_position(conn, collection_id)?;
    let mut added = 0u32;

    for &track_id in track_ids {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM collection_tracks
                WHERE collection_id = ?1 AND track_id = ?2
             )",
            rusqlite::params![collection_id, track_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if exists {
            continue;
        }

        conn.execute(
            "INSERT INTO collection_tracks (collection_id, track_id, position)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![collection_id, track_id, next_position],
        )?;
        next_position += 1;
        added += 1;
    }

    Ok(added)
}

fn next_collection_position(conn: &Connection, collection_id: i64) -> Result<u32> {
    let (max_position, member_count): (Option<u32>, u32) = conn.query_row(
        "SELECT MAX(position), COUNT(*) FROM collection_tracks WHERE collection_id = ?1",
        [collection_id],
        |row| Ok((row.get(0)?, row.get::<_, u32>(1)?)),
    )?;
    Ok(max_position.unwrap_or(0).max(member_count) + 1)
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
    /// MusicBrainz release MBID for this album. Despite the bare name this
    /// holds the *release* id (specific edition), not the release-group id —
    /// the importer only captures the release it matched against. Used as
    /// the key for Cover Art Archive lookups.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn order_clause(self) -> &'static str {
        match self {
            Self::Artist => "a.album_artist COLLATE NOCASE",
            Self::Album => "a.title COLLATE NOCASE",
            Self::Year => "a.year",
            Self::TrackCount => "track_count",
        }
    }
}

/// List albums with pagination and sorting. Cover-art paths in returned
/// rows are resolved to absolute filesystem paths against `music_dir`.
pub fn list_albums(
    conn: &Connection,
    music_dir: &Path,
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
    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
        map_album_row(row, music_dir)
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Count truly loose tracks (tracks with no album and no collection).
pub fn count_loose_tracks(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tracks t
         WHERE t.album_id IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM collection_tracks ct WHERE ct.track_id = t.id
           )",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// List truly loose track ids.
pub fn list_loose_track_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM tracks t
         WHERE t.album_id IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM collection_tracks ct WHERE ct.track_id = t.id
           )
         ORDER BY title COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// List truly loose tracks with pagination. `file_path` in each returned
/// row is the resolved absolute path.
pub fn list_loose_tracks(
    conn: &Connection,
    music_dir: &Path,
    offset: usize,
    limit: usize,
) -> Result<Vec<TrackRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, artist, track_number, disc_number, duration_ms,
                tag_status, bitrate, file_format, file_path
         FROM tracks t
         WHERE t.album_id IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM collection_tracks ct WHERE ct.track_id = t.id
           )
         ORDER BY title COLLATE NOCASE
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
        map_track_row(row, music_dir)
    })?;
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
pub fn search_albums(
    conn: &Connection,
    music_dir: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<AlbumRow>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| format!("%{}%", escape_like(w)))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // Build one "(a.title LIKE ?n OR a.album_artist LIKE ?n)" clause per term.
    let clauses: Vec<String> = (1..=terms.len())
        .map(|i| format!("(a.title LIKE ?{i} ESCAPE '\\' OR a.album_artist LIKE ?{i} ESCAPE '\\')"))
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
    let rows = stmt.query_map(param_refs.as_slice(), |row| map_album_row(row, music_dir))?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Search tracks using FTS5 or LIKE fallback. Returns up to `limit` results.
pub fn search_tracks(
    conn: &Connection,
    music_dir: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<TrackRow>> {
    let fts_count: i64 =
        match conn.query_row("SELECT COUNT(*) FROM tracks_fts", [], |row| row.get(0)) {
            Ok(count) => count,
            Err(e) => {
                tracing::warn!("tracks_fts count failed; falling back to LIKE search: {e}");
                0
            }
        };

    if fts_count > 0 {
        let fts_query = query
            .split_whitespace()
            .filter_map(|w| {
                let term = w.replace('"', "");
                (!term.is_empty()).then(|| format!("\"{}\"*", term))
            })
            .collect::<Vec<_>>()
            .join(" ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT t.id, t.title, t.artist, t.track_number, t.disc_number, t.duration_ms,
                    t.tag_status, t.bitrate, t.file_format, t.file_path
             FROM tracks_fts fts
             JOIN tracks t ON t.id = fts.rowid
             WHERE tracks_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
            map_track_row(row, music_dir)
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    } else {
        let pattern = format!("%{}%", escape_like(query));
        let mut stmt = conn.prepare(
            "SELECT t.id, t.title, t.artist, t.track_number, t.disc_number, t.duration_ms,
                    t.tag_status, t.bitrate, t.file_format, t.file_path
             FROM tracks t
             LEFT JOIN albums a ON a.id = t.album_id
             WHERE t.title LIKE ?1 ESCAPE '\\'
                OR t.artist LIKE ?1 ESCAPE '\\'
                OR a.title LIKE ?1 ESCAPE '\\'
             ORDER BY t.title COLLATE NOCASE
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
            map_track_row(row, music_dir)
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }
}

fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Get an album by ID. Cover-art path is resolved to absolute.
pub fn get_album(conn: &Connection, music_dir: &Path, album_id: i64) -> Result<Option<AlbumRow>> {
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
            |row| map_album_row(row, music_dir),
        )
        .optional()?;
    Ok(row)
}

/// Get tracks for an album, ordered by disc/track number. Paths are absolute.
pub fn get_album_tracks(
    conn: &Connection,
    music_dir: &Path,
    album_id: i64,
) -> Result<Vec<TrackRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, artist, track_number, disc_number, duration_ms,
                tag_status, bitrate, file_format, file_path
         FROM tracks WHERE album_id = ?1
         ORDER BY disc_number, track_number, title COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([album_id], |row| map_track_row(row, music_dir))?;
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

struct CollectionTrackSortRow {
    track: TrackRow,
    explicit_position: Option<u32>,
    album_id: Option<i64>,
}

/// Get tracks in a collection. Paths in returned rows are absolute.
pub fn get_collection_tracks(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
    offset: usize,
    limit: usize,
) -> Result<Vec<TrackRow>> {
    let rows = load_collection_track_sort_rows(conn, music_dir, collection_id)?;
    let items: Vec<CollectionOrderItem> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| CollectionOrderItem {
            index: idx,
            track_id: row.track.id,
            explicit_position: row.explicit_position,
            disc_number: row.track.disc_number,
            track_number: row.track.track_number,
            album_id: row.album_id,
            album_key: None,
            added_order: idx,
            title: row.track.title.clone(),
        })
        .collect();
    let ordered = collection_order::ordered_indices(&items);

    let start = offset.min(ordered.len());
    let end = start.saturating_add(limit).min(ordered.len());
    Ok(ordered[start..end]
        .iter()
        .map(|&idx| rows[idx].track.clone())
        .collect())
}

fn load_collection_track_sort_rows(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
) -> Result<Vec<CollectionTrackSortRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.artist, t.track_number, t.disc_number, t.duration_ms,
                t.tag_status, t.bitrate, t.file_format, t.file_path,
                ct.position, t.album_id
         FROM collection_tracks ct
         JOIN tracks t ON t.id = ct.track_id
         WHERE ct.collection_id = ?1
         ORDER BY ct.added_at, t.id",
    )?;
    let rows = stmt.query_map([collection_id], |row| {
        let stored: Option<String> = row.get(9)?;
        let file_path = stored
            .map(|s| paths::from_db_path(&s, music_dir).display().to_string())
            .unwrap_or_default();
        Ok(CollectionTrackSortRow {
            track: TrackRow {
                id: row.get(0)?,
                title: row.get(1)?,
                artist: row.get(2)?,
                track_number: row.get(3)?,
                disc_number: row.get::<_, Option<u32>>(4)?.unwrap_or(1),
                duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                tag_status: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                bitrate: row.get(7)?,
                file_format: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                file_path,
            },
            explicit_position: row.get(10)?,
            album_id: row.get(11)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn get_collection_effective_positions(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
) -> Result<HashMap<i64, u32>> {
    let rows = load_collection_track_sort_rows(conn, music_dir, collection_id)?;
    let items: Vec<CollectionOrderItem> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| CollectionOrderItem {
            index: idx,
            track_id: row.track.id,
            explicit_position: row.explicit_position,
            disc_number: row.track.disc_number,
            track_number: row.track.track_number,
            album_id: row.album_id,
            album_key: None,
            added_order: idx,
            title: row.track.title.clone(),
        })
        .collect();
    let effective = collection_order::effective_positions(&items);
    let mut out = HashMap::new();
    for (idx, pos) in effective {
        if let Some(row) = rows.get(idx) {
            out.insert(row.track.id, pos);
        }
    }
    Ok(out)
}

/// Load every `collection_file_path` for a collection, keyed by track_id.
/// Used by the TUI collection detail view to show where each track
/// physically lives (organized collection copy vs the track's main path).
/// Paths are resolved to absolute against `music_dir`.
pub fn get_collection_file_paths(
    conn: &Connection,
    music_dir: &Path,
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
        let (id, stored) = row?;
        map.insert(
            id,
            paths::from_db_path(&stored, music_dir)
                .display()
                .to_string(),
        );
    }
    Ok(map)
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

/// Get a single track by ID. `file_path` in the returned row is absolute.
pub fn get_track(conn: &Connection, music_dir: &Path, track_id: i64) -> Result<Option<TrackRow>> {
    let row = conn
        .query_row(
            "SELECT id, title, artist, track_number, disc_number, duration_ms,
                    tag_status, bitrate, file_format, file_path
             FROM tracks WHERE id = ?1",
            [track_id],
            |row| map_track_row(row, music_dir),
        )
        .optional()?;
    Ok(row)
}

/// Update a track's fields in the database.
/// Whitelist of tracks-table columns that [`update_track_fields`] may modify.
/// Const so it can't be accidentally mutated and is checked at compile time.
const UPDATABLE_FIELDS: &[&str] = &["title", "artist", "track_number", "disc_number"];

pub fn update_track_fields(
    conn: &Connection,
    track_id: i64,
    fields: &[(&str, &str)],
) -> Result<()> {
    for (field, value) in fields {
        if !UPDATABLE_FIELDS.contains(field) {
            continue;
        }
        let sql = format!("UPDATE tracks SET {} = ?1 WHERE id = ?2", field);
        // track_number/disc_number are INTEGER columns; bind a parsed
        // integer (or NULL) instead of the raw string. SQLite's column
        // affinity stores non-numeric strings like "3/12" or "" as TEXT,
        // after which every read of the row fails with InvalidColumnType.
        if matches!(*field, "track_number" | "disc_number") {
            let parsed = parse_leading_u32(value);
            conn.execute(&sql, rusqlite::params![parsed, track_id])?;
        } else {
            conn.execute(&sql, rusqlite::params![value, track_id])?;
        }
    }
    conn.execute(
        "UPDATE tracks SET modified_date = datetime('now') WHERE id = ?1",
        [track_id],
    )?;
    Ok(())
}

/// Parse the leading integer of a tag-style numeric field. Handles plain
/// numbers ("7") and "track/total" values ("3/12"). Anything without
/// leading digits (including empty) maps to None → SQL NULL.
fn parse_leading_u32(value: &str) -> Option<u32> {
    let digits: String = value
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
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

/// Track in a collection with information about its other "homes".
/// Used by collection deletion to decide which tracks would be orphaned.
/// File paths are resolved to absolute against `music_dir`.
#[derive(Debug, Clone)]
pub struct CollectionTrackHomes {
    pub track_id: i64,
    pub track_file_path: String,
    pub collection_file_path: Option<String>,
    pub has_album: bool,
    pub other_collection_count: u32,
}

/// For each track in a collection, return its file paths and how many other
/// "homes" it has (an album, or other collection memberships).
pub fn get_collection_tracks_with_other_homes(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
) -> Result<Vec<CollectionTrackHomes>> {
    let mut stmt = conn.prepare(
        "SELECT
            t.id,
            t.file_path,
            ct.collection_file_path,
            (t.album_id IS NOT NULL) as has_album,
            (SELECT COUNT(*) FROM collection_tracks ct2
             WHERE ct2.track_id = t.id AND ct2.collection_id != ?1) as other_collection_count
         FROM collection_tracks ct
         JOIN tracks t ON t.id = ct.track_id
         WHERE ct.collection_id = ?1",
    )?;
    let rows = stmt.query_map([collection_id], |row| {
        let track_path: String = row.get(1)?;
        let coll_path: Option<String> = row.get(2)?;
        Ok(CollectionTrackHomes {
            track_id: row.get(0)?,
            track_file_path: paths::from_db_path(&track_path, music_dir)
                .display()
                .to_string(),
            collection_file_path: coll_path
                .map(|s| paths::from_db_path(&s, music_dir).display().to_string()),
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

/// Clear a track's album membership without deleting the track.
pub fn clear_track_album(conn: &Connection, track_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET album_id = NULL, modified_date = datetime('now') WHERE id = ?1",
        [track_id],
    )?;
    Ok(())
}

/// Info about a track needed when computing a delete plan — the primary file
/// path, album (if any), and every collection-copy path the track owns.
/// All paths are absolute (resolved against `music_dir`).
#[derive(Debug, Clone)]
pub struct TrackDeleteInfo {
    pub track_id: i64,
    pub file_path: String,
    pub album_id: Option<i64>,
    pub collection_count: u32,
    /// Every `collection_tracks.collection_file_path` this track has set.
    pub collection_copies: Vec<String>,
}

/// Load delete-planning info for a set of track ids.
pub fn get_tracks_delete_info(
    conn: &Connection,
    music_dir: &Path,
    track_ids: &[i64],
) -> Result<Vec<TrackDeleteInfo>> {
    if track_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = track_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, file_path, album_id,
                (SELECT COUNT(*) FROM collection_tracks ct WHERE ct.track_id = tracks.id)
         FROM tracks WHERE id IN ({})",
        placeholders
    );
    let params: Vec<&dyn rusqlite::ToSql> = track_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        let stored: Option<String> = row.get(1)?;
        let file_path = stored
            .map(|s| paths::from_db_path(&s, music_dir).display().to_string())
            .unwrap_or_default();
        Ok(TrackDeleteInfo {
            track_id: row.get(0)?,
            file_path,
            album_id: row.get(2)?,
            collection_count: row.get::<_, i64>(3)? as u32,
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
            info.collection_copies
                .push(paths::from_db_path(&c?, music_dir).display().to_string());
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
    let sql = format!("SELECT id FROM tracks WHERE album_id IN ({})", placeholders);
    let params: Vec<&dyn rusqlite::ToSql> = album_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
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
        .optional()?;
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
    artist: &str,
    title: &str,
    year: Option<i32>,
    label: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE albums SET mbid = ?1, album_artist = ?2, title = ?3,
                year = ?4, label = ?5, updated_at = datetime('now')
         WHERE id = ?6",
        rusqlite::params![mbid, artist, title, year, label, album_id],
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
pub struct OrganizeCollectionMembership {
    pub id: i64,
    pub name: String,
    pub path_template: Option<String>,
    pub effective_position: u32,
    /// Absolute path recorded for this collection copy, if any.
    pub collection_file_path: Option<String>,
}

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
    pub collections: Vec<OrganizeCollectionMembership>,
}

/// Get all tracks with album + collection info for organizing.
/// `file_path` on returned rows is absolute.
pub fn get_all_tracks_for_organize(
    conn: &Connection,
    music_dir: &Path,
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
        // Path filtering is applied after DB rows are resolved to filesystem
        // paths. That keeps component boundaries correct (`Artist` does not
        // match `Artist Backup`) and handles the exact `music_dir` root, whose
        // DB-stored prefix is intentionally not representable as an empty path.
        OrganizeFilter::Path(_) => ("1=1".to_string(), vec![]),
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
        let stored: String = row.get(5)?;
        Ok(OrganizeTrackRow {
            id: row.get(0)?,
            title: row.get(1)?,
            artist: row.get(2)?,
            track_number: row.get::<_, Option<u32>>(3)?,
            disc_number: row.get::<_, Option<u32>>(4)?.unwrap_or(1),
            file_path: paths::from_db_path(&stored, music_dir)
                .display()
                .to_string(),
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

    if let OrganizeFilter::Path(prefix) = filter {
        tracks.retain(|track| path_is_same_or_child(Path::new(&track.file_path), prefix));
    }

    // Load collection memberships. Effective positions are computed against
    // the whole collection (not just the filtered organize subset) so
    // collection filenames stay stable when organizing one artist/album.
    let mut effective_cache: HashMap<i64, HashMap<i64, u32>> = HashMap::new();
    let mut coll_stmt = conn.prepare(
        "SELECT ct.collection_id, c.name, c.path_template, ct.position, ct.collection_file_path
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
                row.get::<_, Option<u32>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for c in colls {
            let (id, name, path_template, position, collection_file_path) = c?;
            if let std::collections::hash_map::Entry::Vacant(entry) = effective_cache.entry(id) {
                entry.insert(get_collection_effective_positions(conn, music_dir, id)?);
            }
            let effective_position = effective_cache
                .get(&id)
                .and_then(|positions| positions.get(&track.id).copied())
                .or(position)
                .unwrap_or(0);
            track.collections.push(OrganizeCollectionMembership {
                id,
                name,
                path_template,
                effective_position,
                collection_file_path: collection_file_path
                    .map(|p| paths::from_db_path(&p, music_dir).display().to_string()),
            });
        }
    }

    Ok(tracks)
}

fn path_is_same_or_child(path: &Path, prefix: &Path) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_ok_and(|rel| !rel.as_os_str().is_empty())
}

/// List every (track_id, file_path) currently in the library.
/// Used by the organizer for collision detection. Paths are absolute.
pub fn list_all_track_paths(conn: &Connection, music_dir: &Path) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, file_path FROM tracks")?;
    let rows = stmt.query_map([], |row| {
        let stored: String = row.get(1)?;
        Ok((
            row.get::<_, i64>(0)?,
            paths::from_db_path(&stored, music_dir)
                .display()
                .to_string(),
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// List every recorded collection copy location as
/// `(track_id, collection_id, absolute_path)`. Used by the organizer for
/// collision detection so a move target can't claim another track's
/// collection copy.
pub fn list_all_collection_paths(
    conn: &Connection,
    music_dir: &Path,
) -> Result<Vec<(i64, i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT track_id, collection_id, collection_file_path FROM collection_tracks
         WHERE collection_file_path IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        let stored: String = row.get(2)?;
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            paths::from_db_path(&stored, music_dir)
                .display()
                .to_string(),
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Update a track's file_path. `new_path` is an absolute path; we normalise
/// against `music_dir` before storage.
pub fn update_track_path(
    conn: &Connection,
    music_dir: &Path,
    track_id: i64,
    new_path: &str,
) -> Result<()> {
    let stored = paths::to_db_path(Path::new(new_path), music_dir);
    conn.execute(
        "UPDATE tracks SET file_path = ?1, modified_date = datetime('now') WHERE id = ?2",
        rusqlite::params![stored, track_id],
    )?;
    Ok(())
}

/// Update a collection track's file path. `path` is absolute; normalised
/// against `music_dir` before storage.
pub fn update_collection_track_path(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
    track_id: i64,
    path: &str,
) -> Result<()> {
    let stored = paths::to_db_path(Path::new(path), music_dir);
    conn.execute(
        "UPDATE collection_tracks SET collection_file_path = ?1
         WHERE collection_id = ?2 AND track_id = ?3",
        rusqlite::params![stored, collection_id, track_id],
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

fn map_album_row(row: &rusqlite::Row<'_>, music_dir: &Path) -> rusqlite::Result<AlbumRow> {
    let cover: Option<String> = row.get(10)?;
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
        cover_art_path: cover.map(|s| paths::from_db_path(&s, music_dir).display().to_string()),
    })
}

fn map_track_row(row: &rusqlite::Row<'_>, music_dir: &Path) -> rusqlite::Result<TrackRow> {
    let stored: Option<String> = row.get(9)?;
    let file_path = stored
        .map(|s| paths::from_db_path(&s, music_dir).display().to_string())
        .unwrap_or_default();
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
        file_path,
    })
}

// ---------- Duplicate detection helpers ----------

/// Slim view of an existing track row, as needed by the import-time
/// duplicate picker. Covers just the fields the UI shows + enough identity
/// to act on the row (id, file_path).
#[derive(Debug, Clone)]
#[allow(dead_code)] // album_id/disc_number/duration_ms/mbid surface in v2 picker variants
pub struct ExistingTrackRef {
    pub id: i64,
    pub album_id: Option<i64>,
    pub title: String,
    pub artist: Option<String>,
    pub album_title: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: u32,
    pub duration_ms: Option<u64>,
    pub bitrate: Option<u32>,
    pub file_format: String,
    pub file_path: String,
    pub file_size: Option<i64>,
    pub mbid: Option<String>,
    pub tag_status: String,
}

fn map_existing_track(
    row: &rusqlite::Row<'_>,
    music_dir: &Path,
) -> rusqlite::Result<ExistingTrackRef> {
    let stored: Option<String> = row.get(10)?;
    let file_path = stored
        .map(|s| paths::from_db_path(&s, music_dir).display().to_string())
        .unwrap_or_default();
    Ok(ExistingTrackRef {
        id: row.get(0)?,
        album_id: row.get(1)?,
        title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        artist: row.get(3)?,
        album_title: row.get(4)?,
        track_number: row.get(5)?,
        disc_number: row.get::<_, Option<u32>>(6)?.unwrap_or(1),
        duration_ms: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        bitrate: row.get(8)?,
        file_format: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        file_path,
        file_size: row.get(11)?,
        mbid: row.get(12)?,
        tag_status: row.get::<_, Option<String>>(13)?.unwrap_or_default(),
    })
}

const EXISTING_TRACK_SELECT: &str = "SELECT \
    t.id, t.album_id, t.title, t.artist, a.title AS album_title, \
    t.track_number, t.disc_number, t.duration_ms, t.bitrate, \
    t.file_format, t.file_path, t.file_size, t.mbid, t.tag_status \
    FROM tracks t LEFT JOIN albums a ON a.id = t.album_id";

/// Look up an existing track by its MusicBrainz recording id. Returns the
/// first hit if duplicate MBIDs ever slipped in (shouldn't, but the DB has
/// no unique constraint there so we don't fail hard). Used by the MBID
/// pass in `dup_detect::detect`.
pub fn find_track_by_mbid(
    conn: &Connection,
    music_dir: &Path,
    mbid: &str,
) -> Result<Option<ExistingTrackRef>> {
    let sql = format!("{} WHERE t.mbid = ?1 LIMIT 1", EXISTING_TRACK_SELECT);
    let row = conn
        .query_row(&sql, [mbid], |row| map_existing_track(row, music_dir))
        .optional()?;
    Ok(row)
}

/// Look up an existing track by its position within an album (album + disc
/// + track number). Used as the secondary duplicate signal when MBIDs
///   aren't available on one or both sides.
pub fn find_track_by_album_slot(
    conn: &Connection,
    music_dir: &Path,
    album_id: i64,
    disc_number: u32,
    track_number: u32,
) -> Result<Option<ExistingTrackRef>> {
    let sql = format!(
        "{} WHERE t.album_id = ?1 AND t.disc_number = ?2 AND t.track_number = ?3 LIMIT 1",
        EXISTING_TRACK_SELECT
    );
    let row = conn
        .query_row(
            &sql,
            rusqlite::params![album_id, disc_number, track_number],
            |row| map_existing_track(row, music_dir),
        )
        .optional()?;
    Ok(row)
}

// ---------- Orphaned files ----------

/// Record a file as orphaned — its DB row is gone (or about to go) but the
/// file itself is still on disk, pending cleanup by the organize step.
/// `file_path` is absolute; normalised against `music_dir` before storage.
pub fn insert_orphan(
    conn: &Connection,
    music_dir: &Path,
    file_path: &str,
    title: Option<&str>,
    artist: Option<&str>,
    album_title: Option<&str>,
    reason: &str,
) -> Result<()> {
    let stored = paths::to_db_path(Path::new(file_path), music_dir);
    // Same path might already be tracked as an orphan from an earlier run
    // — OR IGNORE keeps the older row (first-wins), which preserves the
    // original reason/timestamp.
    conn.execute(
        "INSERT OR IGNORE INTO orphaned_files \
            (file_path, title, artist, album_title, reason) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![stored, title, artist, album_title, reason],
    )?;
    Ok(())
}

/// How many orphaned files are currently awaiting cleanup. Staged for
/// inbox / status-bar indicators; `list_orphans` returns the full details
/// for the organize preview.
#[allow(dead_code)]
pub fn count_orphans(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM orphaned_files", [], |row| row.get(0))?;
    Ok(count)
}

/// Single row from the `orphaned_files` table. `id` is the tracking row
/// id (needed to delete it after cleanup), not the (defunct) track id.
#[derive(Debug, Clone)]
pub struct OrphanFileRow {
    pub id: i64,
    pub file_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_title: Option<String>,
    pub reason: String,
}

/// List every file currently tracked as orphaned. Order is insertion
/// order (via `id`) so the preview presents them in the order they
/// were created. `file_path` is absolute.
pub fn list_orphans(conn: &Connection, music_dir: &Path) -> Result<Vec<OrphanFileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_path, title, artist, album_title, reason \
         FROM orphaned_files ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        let stored: String = row.get(1)?;
        Ok(OrphanFileRow {
            id: row.get(0)?,
            file_path: paths::from_db_path(&stored, music_dir)
                .display()
                .to_string(),
            title: row.get(2)?,
            artist: row.get(3)?,
            album_title: row.get(4)?,
            reason: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Remove a tracking row after its file has been handled (deleted, or
/// confirmed already missing). Idempotent — deleting a non-existent id
/// is a no-op, not an error.
pub fn delete_orphan(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM orphaned_files WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::models::{AudioFormat, TagStatus};
    use std::path::PathBuf;

    /// Tests use an empty music_dir — every absolute path stays absolute,
    /// preserving the pre-refactor behaviour these tests were written against.
    fn no_root() -> &'static Path {
        Path::new("")
    }

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

    fn test_track_with(path: &str, title: &str, track_number: Option<u32>) -> Track {
        let mut track = test_track();
        track.file_path = PathBuf::from(path);
        track.title = title.to_string();
        track.track_number = track_number;
        track
    }

    #[test]
    fn test_track_exists_by_path_empty_db() {
        let conn = db::open_memory().unwrap();
        assert!(!track_exists_by_path(&conn, no_root(), "/some/path.mp3").unwrap());
    }

    #[test]
    fn update_track_fields_stores_numeric_columns_as_integers() {
        // ID3-style "3/12" and emptied fields used to be bound as TEXT,
        // poisoning the INTEGER column: every later map_track_row read of
        // the row (and thus the whole album) failed with InvalidColumnType.
        let conn = db::open_memory().unwrap();
        let id = insert_track(&conn, no_root(), &test_track(), None, None).unwrap();

        update_track_fields(&conn, id, &[("track_number", "3/12"), ("disc_number", "")]).unwrap();

        let row = get_track(&conn, no_root(), id)
            .unwrap()
            .expect("row must stay readable after a tag-style numeric update");
        assert_eq!(row.track_number, Some(3), "leading integer of '3/12'");

        // Plain numbers and junk round-trip sanely too.
        update_track_fields(&conn, id, &[("track_number", " 7 ")]).unwrap();
        assert_eq!(
            get_track(&conn, no_root(), id)
                .unwrap()
                .unwrap()
                .track_number,
            Some(7)
        );
        update_track_fields(&conn, id, &[("track_number", "n/a")]).unwrap();
        assert_eq!(
            get_track(&conn, no_root(), id)
                .unwrap()
                .unwrap()
                .track_number,
            None,
            "non-numeric input must become NULL, not TEXT"
        );
    }

    #[test]
    fn parse_leading_u32_cases() {
        assert_eq!(parse_leading_u32("7"), Some(7));
        assert_eq!(parse_leading_u32("3/12"), Some(3));
        assert_eq!(parse_leading_u32(" 04 "), Some(4));
        assert_eq!(parse_leading_u32(""), None);
        assert_eq!(parse_leading_u32("A1"), None);
        assert_eq!(
            parse_leading_u32("99999999999999999999"),
            None,
            "overflow → NULL"
        );
    }

    #[test]
    fn test_insert_and_find_track() {
        let conn = db::open_memory().unwrap();
        let track = test_track();
        let id = insert_track(&conn, no_root(), &track, None, Some(5000)).unwrap();
        assert!(id > 0);
        assert!(track_exists_by_path(&conn, no_root(), "/test/song.mp3").unwrap());
    }

    #[test]
    fn insert_track_persists_mbid() {
        let conn = db::open_memory().unwrap();
        let mut track = test_track();
        track.mbid = Some("recording-mbid".to_string());

        let id = insert_track(&conn, no_root(), &track, None, None).unwrap();
        let stored: Option<String> = conn
            .query_row("SELECT mbid FROM tracks WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(stored.as_deref(), Some("recording-mbid"));
    }

    #[test]
    fn test_insert_album_and_track() {
        let conn = db::open_memory().unwrap();
        let (album_id, created) =
            get_or_create_album(&conn, "Test Album", Some("Artist"), Some(2024), None, 10).unwrap();
        assert!(created);
        let track = test_track();
        let track_id = insert_track(&conn, no_root(), &track, Some(album_id), None).unwrap();
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

        // Case-insensitive match returns the existing row too.
        let (coll_id3, created3) = get_or_create_collection(&conn, "my playlist").unwrap();
        assert_eq!(coll_id, coll_id3);
        assert!(!created3);

        // Add a track
        let track = test_track();
        let track_id = insert_track(&conn, no_root(), &track, None, None).unwrap();
        assert!(add_track_to_collection(&conn, coll_id, track_id).unwrap());

        // Adding again should not error or report a new membership.
        assert!(!add_track_to_collection(&conn, coll_id, track_id).unwrap());
    }

    #[test]
    fn test_collection_positions_append_without_readd_consuming_position() {
        let conn = db::open_memory().unwrap();
        let (coll_id, _) = get_or_create_collection(&conn, "Mix").unwrap();
        let t1 = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/one.mp3", "One", Some(1)),
            None,
            None,
        )
        .unwrap();
        let t2 = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/two.mp3", "Two", Some(2)),
            None,
            None,
        )
        .unwrap();

        assert!(add_track_to_collection(&conn, coll_id, t1).unwrap());
        assert!(!add_track_to_collection(&conn, coll_id, t1).unwrap());
        assert!(add_track_to_collection(&conn, coll_id, t2).unwrap());

        let positions: Vec<(i64, u32)> = conn
            .prepare(
                "SELECT track_id, position FROM collection_tracks
                 WHERE collection_id = ?1 ORDER BY position",
            )
            .unwrap()
            .query_map([coll_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(positions, vec![(t1, 1), (t2, 2)]);
    }

    #[test]
    fn test_collection_tracks_use_explicit_position_order() {
        let conn = db::open_memory().unwrap();
        let (coll_id, _) = get_or_create_collection(&conn, "Mix").unwrap();
        let t1 = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/a.mp3", "A", Some(1)),
            None,
            None,
        )
        .unwrap();
        let t2 = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/b.mp3", "B", Some(2)),
            None,
            None,
        )
        .unwrap();
        add_tracks_to_collection_ordered(&conn, coll_id, &[t1, t2]).unwrap();
        conn.execute(
            "UPDATE collection_tracks
             SET position = CASE track_id WHEN ?1 THEN 2 WHEN ?2 THEN 1 END
             WHERE collection_id = ?3",
            rusqlite::params![t1, t2, coll_id],
        )
        .unwrap();

        let tracks = get_collection_tracks(&conn, no_root(), coll_id, 0, 10).unwrap();
        assert_eq!(
            tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![t2, t1]
        );
    }

    #[test]
    fn test_collection_tracks_legacy_cohesive_metadata_order() {
        let conn = db::open_memory().unwrap();
        let (album_id, _) =
            get_or_create_album(&conn, "Album", Some("Artist"), None, None, 2).unwrap();
        let (coll_id, _) = get_or_create_collection(&conn, "Mix").unwrap();
        let t2 = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/two.mp3", "Two", Some(2)),
            Some(album_id),
            None,
        )
        .unwrap();
        let t1 = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/one.mp3", "One", Some(1)),
            Some(album_id),
            None,
        )
        .unwrap();
        add_tracks_to_collection_ordered(&conn, coll_id, &[t2, t1]).unwrap();
        conn.execute(
            "UPDATE collection_tracks SET position = NULL WHERE collection_id = ?1",
            [coll_id],
        )
        .unwrap();

        let tracks = get_collection_tracks(&conn, no_root(), coll_id, 0, 10).unwrap();
        assert_eq!(
            tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![t1, t2]
        );
    }

    #[test]
    fn test_collection_tracks_legacy_scrambled_metadata_uses_add_order_not_title() {
        let conn = db::open_memory().unwrap();
        let (coll_id, _) = get_or_create_collection(&conn, "Mix").unwrap();
        let zulu = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/z.mp3", "Zulu", Some(7)),
            None,
            None,
        )
        .unwrap();
        let alpha = insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/a.mp3", "Alpha", Some(2)),
            None,
            None,
        )
        .unwrap();
        add_tracks_to_collection_ordered(&conn, coll_id, &[zulu, alpha]).unwrap();
        conn.execute(
            "UPDATE collection_tracks SET position = NULL WHERE collection_id = ?1",
            [coll_id],
        )
        .unwrap();

        let tracks = get_collection_tracks(&conn, no_root(), coll_id, 0, 10).unwrap();
        assert_eq!(
            tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![zulu, alpha]
        );
    }

    #[test]
    fn search_tracks_ignores_all_quote_fts_term() {
        let conn = db::open_memory().unwrap();
        insert_track(&conn, no_root(), &test_track(), None, None).unwrap();

        let rows = search_tracks(&conn, no_root(), "\"", 10).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn like_fallback_escapes_wildcards_and_searches_album_titles() {
        let conn = db::open_memory().unwrap();
        let (album_id, _) =
            get_or_create_album(&conn, "100% Hits", Some("Artist"), None, None, 1).unwrap();
        insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/exact.mp3", "Exact", Some(1)),
            Some(album_id),
            None,
        )
        .unwrap();
        insert_track(
            &conn,
            no_root(),
            &test_track_with("/test/near.mp3", "1000 Nights", Some(2)),
            None,
            None,
        )
        .unwrap();
        conn.execute("DELETE FROM tracks_fts", []).unwrap();

        let rows = search_tracks(&conn, no_root(), "100%", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Exact");
    }

    #[test]
    fn test_count_tracks() {
        let conn = db::open_memory().unwrap();
        assert_eq!(count_tracks(&conn).unwrap(), 0);

        let track = test_track();
        insert_track(&conn, no_root(), &track, None, None).unwrap();
        assert_eq!(count_tracks(&conn).unwrap(), 1);
    }
}
