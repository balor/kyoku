use rusqlite::Connection;

use crate::error::Result;

/// Current schema version.
const SCHEMA_VERSION: i32 = 4;

/// Initialize the database schema. Creates tables if they don't exist
/// and runs any pending migrations.
pub fn initialize(conn: &Connection) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_fresh_db() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

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
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
