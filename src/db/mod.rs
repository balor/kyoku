pub mod models;
pub mod queries;
pub mod schema;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// Open (or create) the library database at the given path and initialize the schema.
pub fn open_database(path: impl AsRef<Path>) -> Result<Connection> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    schema::initialize(&conn)?;
    Ok(conn)
}

/// Open an in-memory database (for testing).
#[allow(dead_code)] // used by unit + integration tests
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    schema::initialize(&conn)?;
    Ok(conn)
}
