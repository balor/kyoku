use rusqlite::Connection;

use crate::error::Result;

/// Plan a path relocation: returns (track_id, old_path, new_path) for all
/// tracks whose file_path starts with `old_prefix`.
pub fn plan_relocate(
    conn: &Connection,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_path FROM tracks WHERE file_path LIKE ?1 || '%'",
    )?;
    let rows = stmt.query_map([old_prefix], |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        Ok((id, path))
    })?;

    let mut plan = Vec::new();
    for row in rows {
        let (id, old_path) = row?;
        let new_path = format!("{}{}", new_prefix, &old_path[old_prefix.len()..]);
        plan.push((id, old_path, new_path));
    }
    Ok(plan)
}

/// Apply a relocation plan: update all file_path values in the DB.
pub fn apply_relocate(conn: &Connection, plan: &[(i64, String, String)]) -> Result<u32> {
    let tx = conn.unchecked_transaction()?;
    let mut count = 0u32;
    for (id, _old, new_path) in plan {
        tx.execute(
            "UPDATE tracks SET file_path = ?1, modified_date = datetime('now') WHERE id = ?2",
            rusqlite::params![new_path, id],
        )?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

/// Verify all track paths exist on disk. Returns (track_id, path) for missing files.
pub fn verify_paths(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, file_path FROM tracks")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        Ok((id, path))
    })?;

    let mut missing = Vec::new();
    for row in rows {
        let (id, path) = row?;
        if !std::path::Path::new(&path).exists() {
            missing.push((id, path));
        }
    }
    Ok(missing)
}
