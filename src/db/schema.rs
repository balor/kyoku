use std::path::Path;

use rusqlite::Connection;

use crate::core::paths;
use crate::error::Result;

/// Current schema version.
const SCHEMA_VERSION: i32 = 7;

/// Initialize the database schema. Creates tables if they don't exist
/// and runs any pending migrations. `music_dir` is needed by the v7
/// migration to rewrite absolute paths under the library root to their
/// relative form. An empty `music_dir` skips that conversion — useful in
/// tests and in fresh setups where there's nothing to rewrite yet.
pub fn initialize(conn: &Connection, music_dir: &Path) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    let version = get_schema_version(conn)?;
    if version < 1 {
        apply_v1(conn)?;
    }
    if version < 2 {
        apply_v2(conn)?;
    }
    if version < 3 {
        apply_v3(conn)?;
    }
    if version < 4 {
        apply_v4(conn)?;
    }
    if version < 5 {
        apply_v5(conn)?;
    }
    if version < 6 {
        apply_v6(conn)?;
    }
    if version < 7 {
        apply_v7(conn, music_dir)?;
    }
    set_schema_version(conn, SCHEMA_VERSION)?;

    Ok(())
}

fn get_schema_version(conn: &Connection) -> Result<i32> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok(version)
}

fn set_schema_version(conn: &Connection, version: i32) -> Result<()> {
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

fn apply_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../migrations/001_initial.sql"))?;
    Ok(())
}

fn apply_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../migrations/002_fts_triggers.sql"))?;
    Ok(())
}

fn apply_v3(conn: &Connection) -> Result<()> {
    // `release_mbid` was removed from the v1 schema file at the same time
    // this migration was introduced, so a brand-new DB goes straight to
    // v3 and never has the column. Only drop when the column is still
    // there (i.e. an existing DB migrating up from v1/v2). SQLite has no
    // `DROP COLUMN IF EXISTS`, so we probe `PRAGMA table_info` first.
    let has_column: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(albums)")?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        names
            .filter_map(|r| r.ok())
            .any(|n| n == "release_mbid")
    };
    if has_column {
        conn.execute_batch(include_str!("../../migrations/003_drop_release_mbid.sql"))?;
    }
    Ok(())
}

fn apply_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../migrations/004_orphaned_files.sql"))?;
    Ok(())
}

fn apply_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../migrations/005_fix_fts_schema.sql"))?;
    Ok(())
}

fn apply_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../../migrations/006_fix_fts_delete_trigger.sql"))?;
    Ok(())
}

/// Rewrite every DB-stored absolute path that lives under `music_dir` to
/// its relative form. Paths outside the library root are left untouched
/// (they stay absolute by design — inbox files, user-relocated copies).
/// Touches `tracks.file_path`, `collection_tracks.collection_file_path`,
/// `albums.cover_art_path`, and `orphaned_files.file_path`.
///
/// Inspired by beets v2.10 (see `beetbox/beets#133`). The point is that
/// after the rewrite, renaming `music_dir` only requires editing the
/// config — every relative row resolves under the new prefix automatically.
fn apply_v7(conn: &Connection, music_dir: &Path) -> Result<()> {
    if music_dir.as_os_str().is_empty() {
        // No library root configured — nothing to rebase.
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;
    rewrite_column(&tx, "tracks", "file_path", "id", music_dir, false)?;
    rewrite_column(
        &tx,
        "collection_tracks",
        "collection_file_path",
        "rowid",
        music_dir,
        true,
    )?;
    rewrite_column(&tx, "albums", "cover_art_path", "id", music_dir, true)?;
    rewrite_column(&tx, "orphaned_files", "file_path", "id", music_dir, false)?;
    tx.commit()?;
    Ok(())
}

fn rewrite_column(
    conn: &Connection,
    table: &str,
    column: &str,
    pk: &str,
    music_dir: &Path,
    nullable: bool,
) -> Result<()> {
    let select_sql = if nullable {
        format!("SELECT {pk}, {column} FROM {table} WHERE {column} IS NOT NULL")
    } else {
        format!("SELECT {pk}, {column} FROM {table}")
    };
    let mut stmt = conn.prepare(&select_sql)?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let update_sql = format!("UPDATE {table} SET {column} = ?1 WHERE {pk} = ?2");
    for (id, old) in rows {
        let new = paths::to_db_path(Path::new(&old), music_dir);
        if new != old {
            conn.execute(&update_sql, rusqlite::params![new, id])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_fresh_db() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn, Path::new("")).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Verify tables exist
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tracks'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_initialize_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn, Path::new("")).unwrap();
        initialize(&conn, Path::new("")).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    /// Existing DBs created before v7 hold absolute paths under music_dir.
    /// After migration those rows must be relative; rows outside music_dir
    /// must stay absolute.
    #[test]
    fn v7_rewrites_absolute_paths_under_music_dir() {
        let conn = Connection::open_in_memory().unwrap();
        // Pre-v7 state: bring the DB up to v6 only.
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_v1(&conn).unwrap();
        apply_v2(&conn).unwrap();
        apply_v3(&conn).unwrap();
        apply_v4(&conn).unwrap();
        apply_v5(&conn).unwrap();
        apply_v6(&conn).unwrap();

        conn.execute(
            "INSERT INTO tracks (title, file_path, file_format, disc_number, tag_status)
             VALUES ('A', '/home/user/Music/Artist/01.mp3', 'mp3', 1, 'unmatched'),
                    ('B', '/elsewhere/inbox/02.mp3', 'mp3', 1, 'unmatched')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO albums (title, cover_art_path)
             VALUES ('Album', '/home/user/Music/Artist/cover.jpg')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO orphaned_files (file_path, reason)
             VALUES ('/home/user/Music/Old/x.mp3', 'replaced'),
                    ('/outside/y.mp3', 'replaced')",
            [],
        )
        .unwrap();

        apply_v7(&conn, Path::new("/home/user/Music")).unwrap();

        let paths: Vec<String> = conn
            .prepare("SELECT file_path FROM tracks ORDER BY title")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec!["Artist/01.mp3", "/elsewhere/inbox/02.mp3"]);

        let cover: String = conn
            .query_row("SELECT cover_art_path FROM albums", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cover, "Artist/cover.jpg");

        let orphans: Vec<String> = conn
            .prepare("SELECT file_path FROM orphaned_files ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(orphans, vec!["Old/x.mp3", "/outside/y.mp3"]);
    }
}
