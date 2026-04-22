-- kyoku schema v1

CREATE TABLE IF NOT EXISTS albums (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    title           TEXT NOT NULL,
    album_artist    TEXT,
    year            INTEGER,
    mbid            TEXT UNIQUE,
    disc_total      INTEGER DEFAULT 1,
    track_total     INTEGER,
    genre           TEXT,
    label           TEXT,
    media_type      TEXT,
    album_type      TEXT DEFAULT 'album',
    cover_art_path  TEXT,
    created_at      TEXT DEFAULT (datetime('now')),
    updated_at      TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS tracks (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id        INTEGER REFERENCES albums(id),
    title           TEXT NOT NULL,
    artist          TEXT,
    track_number    INTEGER,
    disc_number     INTEGER DEFAULT 1,
    duration_ms     INTEGER,
    mbid            TEXT,

    -- File info
    file_path       TEXT NOT NULL UNIQUE,
    file_size       INTEGER,
    file_format     TEXT,
    bitrate         INTEGER,
    sample_rate     INTEGER,
    channels        INTEGER,

    -- Original filesystem context
    source_dir      TEXT,

    -- Fingerprint
    acoustid        TEXT,
    chromaprint     TEXT,

    -- Metadata state
    tag_status      TEXT DEFAULT 'unmatched',
    import_date     TEXT DEFAULT (datetime('now')),
    modified_date   TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS collections (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    description TEXT,
    path_template TEXT,
    created_at  TEXT DEFAULT (datetime('now')),
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS collection_tracks (
    collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    track_id      INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    position      INTEGER,
    collection_file_path TEXT,
    added_at      TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (collection_id, track_id)
);

-- Full-text search
CREATE VIRTUAL TABLE IF NOT EXISTS tracks_fts USING fts5(
    title, artist, album_title,
    content='tracks',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album_id);
CREATE INDEX IF NOT EXISTS idx_tracks_path ON tracks(file_path);
CREATE INDEX IF NOT EXISTS idx_tracks_loose ON tracks(album_id) WHERE album_id IS NULL;
CREATE INDEX IF NOT EXISTS idx_albums_year ON albums(year);
CREATE INDEX IF NOT EXISTS idx_albums_type ON albums(album_type);
CREATE INDEX IF NOT EXISTS idx_tracks_status ON tracks(tag_status);
CREATE INDEX IF NOT EXISTS idx_collection_tracks_coll ON collection_tracks(collection_id);
CREATE INDEX IF NOT EXISTS idx_collection_tracks_track ON collection_tracks(track_id);
