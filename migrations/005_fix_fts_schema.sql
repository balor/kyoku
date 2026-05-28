-- Fix the broken tracks_fts external-content mapping.
--
-- tracks_fts was created with content='tracks', but 'tracks' has no
-- 'album_title' column. FTS5 implicitly joins the content table on
-- SELECT / introspection, causing 'no such column: album_title'.
-- We rebuild as a self-contained FTS5 table (triggers already manage
-- it manually) and force a clean index.

-- 1. Remove the old (broken) FTS table and its shadow tables
DROP TABLE IF EXISTS tracks_fts;

-- 2. Recreate without the external content mapping
CREATE VIRTUAL TABLE tracks_fts USING fts5(
    title,
    artist,
    album_title,
    tokenize='unicode61 remove_diacritics 2'
);

-- 3. Drop old triggers so we can recreate them cleanly
DROP TRIGGER IF EXISTS tracks_fts_insert;
DROP TRIGGER IF EXISTS tracks_fts_delete;
DROP TRIGGER IF EXISTS tracks_fts_update;

-- 4. Re-create triggers using the simpler, correct syntax
CREATE TRIGGER tracks_fts_insert AFTER INSERT ON tracks BEGIN
    INSERT INTO tracks_fts(rowid, title, artist, album_title)
    VALUES (NEW.id,
            NEW.title,
            NEW.artist,
            (SELECT title FROM albums WHERE id = NEW.album_id));
END;

CREATE TRIGGER tracks_fts_delete AFTER DELETE ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = OLD.id;
END;

CREATE TRIGGER tracks_fts_update AFTER UPDATE ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = OLD.id;
    INSERT INTO tracks_fts(rowid, title, artist, album_title)
    VALUES (NEW.id,
            NEW.title,
            NEW.artist,
            (SELECT title FROM albums WHERE id = NEW.album_id));
END;

-- 5. Rebuild the full-text index from current tracks/albums data
INSERT INTO tracks_fts(rowid, title, artist, album_title)
SELECT t.id, t.title, t.artist, a.title
FROM tracks t
LEFT JOIN albums a ON t.album_id = a.id;
