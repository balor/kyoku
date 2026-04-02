use rusqlite::Connection;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;

    #[test]
    fn test_track_exists_by_path_empty_db() {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialize(&conn).unwrap();

        assert!(!track_exists_by_path(&conn, "/some/path.mp3").unwrap());
    }
}
