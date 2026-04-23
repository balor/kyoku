-- Files that a user elected to "replace" during an import-time duplicate
-- resolution. Their DB row has already been removed, but the file on disk
-- is left alone so the user can inspect/back-up before the next organize
-- pass sweeps them. The organize step is the canonical place where we
-- actually delete these files from disk.
CREATE TABLE IF NOT EXISTS orphaned_files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path   TEXT NOT NULL UNIQUE,
    -- Snapshot of identifying info for surfacing in UIs after the track
    -- row is gone. All nullable — not every orphan came from an MB-matched
    -- row or had a populated title.
    title       TEXT,
    artist      TEXT,
    album_title TEXT,
    -- Short human-readable reason, e.g. "replaced by duplicate during import".
    reason      TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_orphaned_files_created
    ON orphaned_files(created_at);
