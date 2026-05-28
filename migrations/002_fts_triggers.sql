-- FTS5 sync triggers for automatic index maintenance

-- Populate FTS from existing data
INSERT INTO tracks_fts(rowid, title, artist, album_title)
SELECT t.id, t.title, t.artist, a.title
FROM tracks t LEFT JOIN albums a ON t.album_id = a.id;

-- Auto-sync on INSERT
CREATE TRIGGER tracks_fts_insert AFTER INSERT ON tracks BEGIN
    INSERT INTO tracks_fts(rowid, title, artist, album_title)
    VALUES (NEW.id, NEW.title, NEW.artist,
            (SELECT title FROM albums WHERE id = NEW.album_id));
END;

-- Auto-sync on DELETE
CREATE TRIGGER tracks_fts_delete AFTER DELETE ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = OLD.id;
END;

-- Auto-sync on UPDATE
CREATE TRIGGER tracks_fts_update AFTER UPDATE ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = OLD.id;
    INSERT INTO tracks_fts(rowid, title, artist, album_title)
    VALUES (NEW.id, NEW.title, NEW.artist,
            (SELECT title FROM albums WHERE id = NEW.album_id));
END;
