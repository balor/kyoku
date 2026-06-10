pub mod models;
pub mod queries;
pub mod schema;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// Open (or create) the library database at the given path and initialize the schema.
/// `music_dir` is forwarded to the v7 migration so legacy absolute paths
/// under the library root get rewritten to their relative form. Pass an
/// empty path when there's no configured library root yet (fresh setup) —
/// nothing exists to rebase.
pub fn open_database(path: impl AsRef<Path>, music_dir: &Path) -> Result<Connection> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    schema::initialize(&conn, music_dir)?;
    Ok(conn)
}

/// Open an in-memory database (for testing). Initialized with no library
/// root, so the v7 path-rewrite migration is a no-op.
#[allow(dead_code)] // used by unit + integration tests
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    schema::initialize(&conn, Path::new(""))?;
    Ok(conn)
}
