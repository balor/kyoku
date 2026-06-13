-- Schema v8: FTS album-title sync, lookup indexes, collection name uniqueness.

CREATE TRIGGER IF NOT EXISTS albums_fts_title_update
AFTER UPDATE OF title ON albums
WHEN NEW.title IS NOT OLD.title
BEGIN
    UPDATE tracks_fts SET album_title = NEW.title
    WHERE rowid IN (SELECT id FROM tracks WHERE album_id = NEW.id);
END;

CREATE INDEX IF NOT EXISTS idx_tracks_mbid ON tracks(mbid) WHERE mbid IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_albums_title_artist ON albums(title, album_artist);
DROP INDEX IF EXISTS idx_tracks_path;

CREATE UNIQUE INDEX IF NOT EXISTS idx_collections_name_nocase
    ON collections(name COLLATE NOCASE);
