use rusqlite::Connection;

use crate::error::Result;

/// Plan a path relocation: returns (track_id, old_path, new_path) for all
/// tracks whose file_path is exactly `old_prefix` or lives under it as a
/// directory child. Trailing slashes on either prefix are trimmed so that
/// `/old/music` and `/old/music/` behave identically, and paths like
/// `/old/music_backup/...` are NOT matched (strict path-component boundary).
pub fn plan_relocate(
    conn: &Connection,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<Vec<(i64, String, String)>> {
    let old_trimmed = old_prefix.trim_end_matches('/');
    let new_trimmed = new_prefix.trim_end_matches('/');

    // SQL LIKE pre-filter (fast, uses the index). Escape wildcards in the
    // user-provided prefix so a literal `%` or `_` doesn't silently match.
    let like_pattern = format!("{}%", escape_like(old_trimmed));
    let mut stmt = conn.prepare(
        "SELECT id, file_path FROM tracks WHERE file_path LIKE ?1 ESCAPE '\\'",
    )?;
    let rows = stmt.query_map([&like_pattern], |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        Ok((id, path))
    })?;

    let mut plan = Vec::new();
    for row in rows {
        let (id, old_path) = row?;
        // Strict boundary check in Rust — guards against overlapping prefixes
        // like `/old/music` matching `/old/music_backup/...`.
        let suffix = match old_path.strip_prefix(old_trimmed) {
            Some(s) if s.is_empty() || s.starts_with('/') => s,
            _ => continue,
        };
        let new_path = format!("{new_trimmed}{suffix}");
        plan.push((id, old_path, new_path));
    }
    Ok(plan)
}

/// Escape SQL LIKE metacharacters (`%`, `_`, `\`) so a user-provided string
/// is treated as a literal. Pair with `ESCAPE '\'` on the LIKE clause.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::TempDir;

    /// Insert a bare-minimum track row with `file_path`. Returns track id.
    fn insert_path(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO tracks (title, file_path, file_format, disc_number, tag_status)
             VALUES ('t', ?1, 'mp3', 1, 'unmatched')",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn fetch_path(conn: &Connection, id: i64) -> String {
        conn.query_row(
            "SELECT file_path FROM tracks WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn plan_relocate_returns_only_paths_under_old_prefix() {
        let conn = db::open_memory().unwrap();
        let a = insert_path(&conn, "/old/music/artist/a.mp3");
        let b = insert_path(&conn, "/old/music/artist/b.mp3");
        let _c = insert_path(&conn, "/somewhere/else/c.mp3");

        let plan = plan_relocate(&conn, "/old/music", "/new/library").unwrap();

        assert_eq!(plan.len(), 2);
        let ids: Vec<i64> = plan.iter().map(|(id, _, _)| *id).collect();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
        // Rewrite preserves suffix after the prefix.
        for (_, old, new) in &plan {
            assert!(old.starts_with("/old/music"));
            assert!(new.starts_with("/new/library"));
            assert_eq!(&new[..], &format!("/new/library{}", &old["/old/music".len()..]));
        }
    }

    #[test]
    fn apply_relocate_updates_all_matching_paths_atomically() {
        let conn = db::open_memory().unwrap();
        let a = insert_path(&conn, "/old/music/x/a.mp3");
        let b = insert_path(&conn, "/old/music/x/b.mp3");
        let c = insert_path(&conn, "/elsewhere/c.mp3");

        let plan = plan_relocate(&conn, "/old/music", "/new/library").unwrap();
        let count = apply_relocate(&conn, &plan).unwrap();

        assert_eq!(count, 2);
        assert_eq!(fetch_path(&conn, a), "/new/library/x/a.mp3");
        assert_eq!(fetch_path(&conn, b), "/new/library/x/b.mp3");
        // Paths outside the prefix are untouched.
        assert_eq!(fetch_path(&conn, c), "/elsewhere/c.mp3");
    }

    #[test]
    fn plan_relocate_rejects_overlapping_prefix() {
        // `/old/music` must NOT match `/old/music_backup/...` — the boundary
        // check ensures only exact matches or directory children are included.
        let conn = db::open_memory().unwrap();
        let a = insert_path(&conn, "/old/music/a.mp3");
        let _b = insert_path(&conn, "/old/music_backup/b.mp3");
        let _c = insert_path(&conn, "/old/musicalbum.mp3");

        let plan = plan_relocate(&conn, "/old/music", "/new/library").unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].0, a);
    }

    #[test]
    fn plan_relocate_trims_trailing_slashes() {
        let conn = db::open_memory().unwrap();
        let a = insert_path(&conn, "/old/music/a.mp3");

        let with_slash = plan_relocate(&conn, "/old/music/", "/new/library/").unwrap();
        assert_eq!(with_slash.len(), 1);
        assert_eq!(with_slash[0].0, a);
        assert_eq!(with_slash[0].2, "/new/library/a.mp3");
    }

    #[test]
    fn plan_relocate_escapes_like_wildcards() {
        // A literal `_` in the old prefix must not act as a SQL wildcard
        // (otherwise `/old/_usic/x` would also match `/old/music/x`).
        let conn = db::open_memory().unwrap();
        let a = insert_path(&conn, "/old/_usic/a.mp3");
        let _b = insert_path(&conn, "/old/music/b.mp3");

        let plan = plan_relocate(&conn, "/old/_usic", "/new/library").unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].0, a);
    }

    #[test]
    fn verify_paths_returns_only_missing_files() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real.mp3");
        std::fs::write(&real, b"").unwrap();
        let ghost = tmp.path().join("ghost.mp3");

        let conn = db::open_memory().unwrap();
        let _real_id = insert_path(&conn, real.to_str().unwrap());
        let ghost_id = insert_path(&conn, ghost.to_str().unwrap());

        let missing = verify_paths(&conn).unwrap();

        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, ghost_id);
        assert_eq!(missing[0].1, ghost.to_str().unwrap());
    }
}
