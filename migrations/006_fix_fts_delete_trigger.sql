-- Fix the broken FTS5 delete/update triggers.
--
-- Migration 005 recreated tracks_fts as self-contained (no content=)
-- but kept the INSERT ... VALUES('delete', ...) syntax which only
-- works for external-content FTS5 tables. On self-contained tables
-- it causes a SQL logic error, silently aborting every DELETE on
-- tracks (and thus album/collection deletions).
--
-- Replace with DELETE FROM tracks_fts WHERE rowid = OLD.id which is
-- the correct syntax for self-contained FTS5 tables.

DROP TRIGGER IF EXISTS tracks_fts_delete;
DROP TRIGGER IF EXISTS tracks_fts_update;

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
