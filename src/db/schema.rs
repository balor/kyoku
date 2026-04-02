use rusqlite::Connection;

use crate::error::Result;

/// Current schema version.
const SCHEMA_VERSION: i32 = 1;

/// Initialize the database schema. Creates tables if they don't exist
/// and runs any pending migrations.
pub fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    let version = get_schema_version(conn)?;
    if version < 1 {
        apply_v1(conn)?;
        set_schema_version(conn, SCHEMA_VERSION)?;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_fresh_db() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, 1);

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
        assert_eq!(version, 1);
    }
}
