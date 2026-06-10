use super::*;
use crate::config::Settings;
use crate::db;
use crate::db::models::{AudioFormat, TagStatus, Track};
use crate::db::queries;
use rusqlite::Connection;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────

/// Settings rooted at `music_dir`. All template defaults retained.
fn settings_with_music_dir(music_dir: &Path) -> Settings {
    let mut s = Settings::default();
    s.library.music_dir = music_dir.to_path_buf();
    s
}

fn touch(path: &Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, b"").unwrap();
}

fn make_track_struct(
    file_path: PathBuf,
    album_id: Option<i64>,
    title: &str,
    artist: &str,
    track_no: u32,
) -> Track {
    Track {
        id: None,
        album_id,
        title: title.into(),
        artist: Some(artist.into()),
        track_number: Some(track_no),
        disc_number: 1,
        duration_ms: Some(180_000),
        mbid: None,
        file_path,
        file_format: AudioFormat::Mp3,
        bitrate: Some(320),
        sample_rate: Some(44_100),
        tag_status: TagStatus::Unmatched,
        source_dir: None,
    }
}

/// Insert an album (if missing) and a single track at `src_path` (touched on disk).
fn add_album_track(
    conn: &Connection,
    src_path: PathBuf,
    artist: &str,
    album: &str,
    track_no: u32,
    title: &str,
    year: Option<i32>,
) -> i64 {
    let (album_id, _) =
        queries::get_or_create_album(conn, album, Some(artist), year, None, 1).unwrap();
    touch(&src_path);
    let track = make_track_struct(src_path, Some(album_id), title, artist, track_no);
    // Tests use absolute paths and an in-memory DB; storage form is
    // pre-organize so the inbox path stays absolute regardless of music_dir.
    queries::insert_track(conn, Path::new(""), &track, Some(album_id), None).unwrap()
}

/// Insert a loose track at `src_path` (touched on disk). No album row.
fn add_loose_track(conn: &Connection, src_path: PathBuf, artist: &str, title: &str) -> i64 {
    touch(&src_path);
    let track = make_track_struct(src_path, None, title, artist, 1);
    queries::insert_track(conn, Path::new(""), &track, None, None).unwrap()
}

fn set_album_disc_total(conn: &Connection, album_id: i64, disc_total: u32) {
    conn.execute(
        "UPDATE albums SET disc_total = ?1 WHERE id = ?2",
        rusqlite::params![disc_total, album_id],
    )
    .unwrap();
}

fn ensure_collection(conn: &Connection, name: &str, track_id: i64) -> i64 {
    let (coll_id, _) = queries::get_or_create_collection(conn, name).unwrap();
    queries::add_track_to_collection(conn, coll_id, track_id).unwrap();
    coll_id
}

/// Standard scratch-dir setup: returns (TempDir, src_dir, music_dir, conn).
fn fresh_world() -> (TempDir, PathBuf, PathBuf, Connection) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("inbox");
    let music = tmp.path().join("music");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&music).unwrap();
    let conn = db::open_memory().unwrap();
    (tmp, src, music, conn)
}

// ── plan_organize: branch coverage ───────────────────────────────

#[test]
fn plan_album_track_uses_single_disc_template_when_disc_total_is_1() {
    let (_tmp, src, music, conn) = fresh_world();
    add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        3,
        "Song",
        Some(2024),
    );

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 1);
    // Default single-disc template: "{album_artist}/{album} ({year})/{track:02} {title}.{ext}"
    assert_eq!(
        plan.moves[0].to,
        music.join("Artist/Album (2024)/03 Song.mp3"),
    );
}

#[test]
fn plan_album_track_uses_multi_disc_template_when_disc_total_gt_1() {
    let (_tmp, src, music, conn) = fresh_world();
    let track_id = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        3,
        "Song",
        Some(2024),
    );
    // Mark the album as multi-disc (track stays on disc 1, see make_track_struct)
    let album_id: i64 = conn
        .query_row(
            "SELECT album_id FROM tracks WHERE id = ?1",
            [track_id],
            |r| r.get(0),
        )
        .unwrap();
    set_album_disc_total(&conn, album_id, 2);

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 1);
    // Default multi-disc template: "{album_artist}/{album} ({year})/{disc:0}-{track:02} {title}.{ext}"
    assert_eq!(
        plan.moves[0].to,
        music.join("Artist/Album (2024)/1-03 Song.mp3"),
    );
}

#[test]
fn plan_album_track_in_one_collection_emits_one_copy() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    ensure_collection(&conn, "Mix", tid);

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 1, "album track should be moved");
    assert_eq!(plan.copies.len(), 1, "one collection copy expected");
    assert_eq!(plan.copies[0].collection_name, "Mix");
    assert!(
        plan.copies[0]
            .to
            .starts_with(music.join("Collections/Mix/")),
        "copy target should land under Collections/Mix/, got {:?}",
        plan.copies[0].to
    );
}

#[test]
fn plan_collection_copy_uses_collection_position_not_track_number() {
    let (_tmp, src, music, conn) = fresh_world();
    let late = add_album_track(
        &conn,
        src.join("late.mp3"),
        "Artist",
        "Album",
        7,
        "Late",
        Some(2024),
    );
    let early = add_album_track(
        &conn,
        src.join("early.mp3"),
        "Artist",
        "Album",
        2,
        "Early",
        Some(2024),
    );
    let (coll_id, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();
    queries::add_tracks_to_collection_ordered(&conn, coll_id, &[late, early]).unwrap();

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let targets: Vec<PathBuf> = plan.copies.iter().map(|c| c.to.clone()).collect();
    assert!(
        targets.contains(&music.join("Collections/Mix/01 Artist - Late.mp3")),
        "collection templates should use collection position; got {:?}",
        targets,
    );
    assert!(targets.contains(&music.join("Collections/Mix/02 Artist - Early.mp3")));
}

#[test]
fn plan_album_track_in_two_collections_emits_two_copies() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    ensure_collection(&conn, "Mix", tid);
    ensure_collection(&conn, "Workout", tid);

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 1);
    assert_eq!(plan.copies.len(), 2);
    let mut names: Vec<&str> = plan
        .copies
        .iter()
        .map(|c| c.collection_name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, vec!["Mix", "Workout"]);
}

#[test]
fn plan_loose_track_no_collection_moves_to_underscore_loose() {
    let (_tmp, src, music, conn) = fresh_world();
    add_loose_track(&conn, src.join("stray.mp3"), "Artist", "Stray");

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 1);
    assert!(plan.copies.is_empty());
    // Default loose template: "_loose/{artist} - {title}.{ext}"
    assert_eq!(plan.moves[0].to, music.join("_loose/Artist - Stray.mp3"));
    assert!(plan.moves[0].also_collection.is_none());
}

#[test]
fn plan_loose_track_in_one_collection_moves_to_collection_folder() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_loose_track(&conn, src.join("song.mp3"), "Artist", "Song");
    let coll_id = ensure_collection(&conn, "Mix", tid);

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(
        plan.moves.len(),
        1,
        "loose-in-one-collection moves, doesn't copy"
    );
    assert!(plan.copies.is_empty());
    assert!(plan.moves[0].to.starts_with(music.join("Collections/Mix/")));
    assert_eq!(
        plan.moves[0].also_collection,
        Some((coll_id, "Mix".into())),
        "the move must also update the collection's primary file path",
    );
}

#[test]
fn plan_loose_track_in_two_collections_moves_to_first_copies_to_second() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_loose_track(&conn, src.join("song.mp3"), "Artist", "Song");
    // Collection IDs come back monotonic; first inserted is "first" by ID
    let first_id = ensure_collection(&conn, "Alpha", tid);
    let _second_id = ensure_collection(&conn, "Beta", tid);

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 1);
    assert_eq!(plan.copies.len(), 1);
    assert_eq!(
        plan.moves[0].also_collection.as_ref().map(|(id, _)| *id),
        Some(first_id),
    );
    assert_eq!(plan.copies[0].collection_name, "Beta");
}

#[test]
fn plan_skips_track_already_at_target_path() {
    let (_tmp, _src, music, conn) = fresh_world();
    // Place the file at exactly the destination the template would produce.
    let target = music.join("Artist/Album (2024)/01 Song.mp3");
    let _tid = add_album_track(&conn, target, "Artist", "Album", 1, "Song", Some(2024));

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert!(plan.moves.is_empty(), "in-place file should not be moved");
    assert_eq!(plan.skipped, 1);
}

#[test]
fn plan_disambiguates_filename_collision_with_numeric_suffix() {
    let (_tmp, src, music, conn) = fresh_world();
    // Two distinct tracks rendering to the same target path: same artist, same
    // album, same track number, same title — different source files.
    add_album_track(
        &conn,
        src.join("a/song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    add_album_track(
        &conn,
        src.join("b/song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.moves.len(), 2);
    let targets: Vec<&Path> = plan.moves.iter().map(|m| m.to.as_path()).collect();
    assert!(targets.contains(&music.join("Artist/Album (2024)/01 Song.mp3").as_path()));
    assert!(
        targets.contains(&music.join("Artist/Album (2024)/01 Song (2).mp3").as_path()),
        "expected disambiguation suffix ' (2)' before extension; got {:?}",
        targets,
    );
}

#[test]
fn plan_detects_missing_source_file_as_orphan() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    // Delete the source file so the DB row is now an orphan
    std::fs::remove_file(src.join("song.mp3")).unwrap();

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert!(plan.moves.is_empty(), "orphan should not be moved");
    assert_eq!(plan.missing_sources.len(), 1);
    assert_eq!(plan.missing_sources[0].0, tid);
}

// ── apply_organize ───────────────────────────────────────────────

#[test]
fn apply_move_relocates_file_and_updates_db_path() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone(), src.clone()]).unwrap();

    assert_eq!(result.moved, 1);
    assert!(result.errors.is_empty());
    let target = music.join("Artist/Album (2024)/01 Song.mp3");
    assert!(target.exists(), "target file should exist after move");
    assert!(
        !src.join("song.mp3").exists(),
        "source should be gone after move"
    );

    // Stored path is now relative-to-music_dir; the queries layer resolves
    // it back to absolute on read, so go through that instead of reading
    // the row directly.
    let row = queries::get_track(&conn, &music, tid).unwrap().unwrap();
    assert_eq!(PathBuf::from(&row.file_path), target);
}

#[test]
fn apply_copy_creates_collection_file_and_updates_collection_tracks_path() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    let coll_id = ensure_collection(&conn, "Mix", tid);
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone(), src.clone()]).unwrap();

    assert_eq!(result.copied, 1);
    // Resolved through the queries layer so the relative storage form is
    // rejoined with music_dir before the on-disk check.
    let coll_paths = queries::get_collection_file_paths(&conn, &music, coll_id).unwrap();
    let coll_path = coll_paths
        .get(&tid)
        .expect("collection_file_path should be set after copy");
    assert!(
        PathBuf::from(coll_path).exists(),
        "collection copy should exist on disk"
    );
    assert!(coll_path.contains("Collections/Mix/"));
}

#[test]
fn apply_move_with_also_collection_updates_both_tracks_and_collection_tracks() {
    let (_tmp, src, music, conn) = fresh_world();
    // Loose track in one collection → move serves as the collection's primary
    let tid = add_loose_track(&conn, src.join("song.mp3"), "Artist", "Song");
    let coll_id = ensure_collection(&conn, "Mix", tid);
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone(), src.clone()]).unwrap();
    assert_eq!(result.moved, 1);
    assert_eq!(
        result.copied, 0,
        "no separate copy expected — the move is the copy"
    );

    let track_path = queries::get_track(&conn, &music, tid).unwrap().unwrap().file_path;
    let coll_paths = queries::get_collection_file_paths(&conn, &music, coll_id).unwrap();
    let coll_path = coll_paths
        .get(&tid)
        .expect("collection_file_path should be set")
        .clone();
    assert_eq!(
        track_path, coll_path,
        "both rows must point at the same file"
    );
    assert!(PathBuf::from(&track_path).exists());
}

#[test]
fn apply_move_cleans_empty_source_directories_walking_up() {
    let (_tmp, src, music, conn) = fresh_world();
    // Source path nested two levels deep — both should be cleaned after move
    add_album_track(
        &conn,
        src.join("nested/deep/song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone(), src.clone()]).unwrap();
    assert_eq!(result.moved, 1);
    assert!(
        result.dirs_cleaned >= 2,
        "deep + nested should both be cleaned, got {}",
        result.dirs_cleaned
    );
    assert!(
        !src.join("nested/deep").exists(),
        "deepest dir should be gone"
    );
    assert!(
        !src.join("nested").exists(),
        "intermediate empty dir should be gone"
    );
}

#[test]
fn apply_deletes_orphan_track_rows() {
    let (_tmp, src, music, conn) = fresh_world();
    let tid = add_album_track(
        &conn,
        src.join("song.mp3"),
        "Artist",
        "Album",
        1,
        "Song",
        Some(2024),
    );
    std::fs::remove_file(src.join("song.mp3")).unwrap();
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();
    assert_eq!(plan.missing_sources.len(), 1);

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone(), src.clone()]).unwrap();

    assert_eq!(result.orphans_cleaned, 1);
    let exists: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(exists, 0, "orphan track row should be deleted");
}

// ── File orphans (orphaned_files table) ──────────────────────────

#[test]
fn plan_picks_up_file_orphans_from_orphaned_files_table() {
    let (_tmp, _src, music, conn) = fresh_world();
    // Simulate a dup-replace leftover: file sits under music_dir with no
    // track row, tracked only in orphaned_files.
    let orphan_path = music.join("Artist/Album (2024)/old.mp3");
    touch(&orphan_path);
    queries::insert_orphan(
        &conn,
        &music,
        &orphan_path.display().to_string(),
        Some("Old Song"),
        Some("Artist"),
        Some("Album"),
        "dup-replace",
    )
    .unwrap();

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    assert_eq!(plan.file_orphans.len(), 1);
    let e = &plan.file_orphans[0];
    assert_eq!(e.path, orphan_path);
    assert_eq!(e.title.as_deref(), Some("Old Song"));
    assert_eq!(e.artist.as_deref(), Some("Artist"));
    assert_eq!(e.album_title.as_deref(), Some("Album"));
    assert_eq!(e.reason, "dup-replace");
}

#[test]
fn plan_includes_file_orphans_regardless_of_filter() {
    let (_tmp, _src, music, conn) = fresh_world();
    let orphan_path = music.join("leftover.mp3");
    touch(&orphan_path);
    queries::insert_orphan(
        &conn,
        &music,
        &orphan_path.display().to_string(),
        None,
        None,
        None,
        "dup-replace",
    )
    .unwrap();

    // A filter that matches no tracks — the orphan should still surface.
    let plan = plan_organize(
        &conn,
        &settings_with_music_dir(&music),
        OrganizeFilter::Artist("Nobody".into()),
    )
    .unwrap();

    assert!(plan.moves.is_empty());
    assert_eq!(plan.file_orphans.len(), 1);
}

#[test]
fn apply_unlinks_orphan_file_and_clears_tracking_row() {
    let (_tmp, _src, music, conn) = fresh_world();
    let orphan_path = music.join("Artist/Album (2024)/old.mp3");
    touch(&orphan_path);
    queries::insert_orphan(
        &conn,
        &music,
        &orphan_path.display().to_string(),
        Some("Old"),
        Some("Artist"),
        Some("Album"),
        "dup-replace",
    )
    .unwrap();
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone()]).unwrap();

    assert_eq!(result.file_orphans_removed, 1);
    assert!(result.errors.is_empty());
    assert!(!orphan_path.exists(), "orphan file should be unlinked");
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM orphaned_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "orphan tracking row should be cleared");
}

#[test]
fn apply_treats_already_missing_orphan_as_success() {
    let (_tmp, _src, music, conn) = fresh_world();
    // Row is there, but the file never made it to disk (or was removed
    // out-of-band). Apply should still clear the tracking row — this is
    // the idempotent path that makes repeated organize runs safe.
    let ghost = music.join("Artist/Album (2024)/ghost.mp3");
    queries::insert_orphan(
        &conn,
        &music,
        &ghost.display().to_string(),
        Some("Ghost"),
        Some("Artist"),
        Some("Album"),
        "dup-replace",
    )
    .unwrap();
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();
    assert_eq!(plan.file_orphans.len(), 1);

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone()]).unwrap();

    assert_eq!(result.file_orphans_removed, 1);
    assert!(result.errors.is_empty());
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM orphaned_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn apply_cleans_empty_parent_directories_after_orphan_unlink() {
    let (_tmp, _src, music, conn) = fresh_world();
    // Orphan is the only file in a nested album dir — after unlink both
    // Album and Artist levels should collapse.
    let orphan_path = music.join("Artist/Album (2024)/old.mp3");
    touch(&orphan_path);
    queries::insert_orphan(
        &conn,
        &music,
        &orphan_path.display().to_string(),
        None,
        None,
        None,
        "dup-replace",
    )
    .unwrap();
    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone()]).unwrap();

    assert_eq!(result.file_orphans_removed, 1);
    assert!(
        result.dirs_cleaned >= 2,
        "Album + Artist should both be cleaned, got {}",
        result.dirs_cleaned
    );
    assert!(!music.join("Artist/Album (2024)").exists());
    assert!(!music.join("Artist").exists());
    assert!(music.exists(), "music_dir itself must survive");
}

/// Regression test for a critical bug where a dup "Keep New" import
/// followed by `organize` could destroy freshly-moved files. The orphan
/// row points at the old library path, but the new import's organize
/// destination resolves to the *same* path — the move overwrites the
/// old file (correct), and then the orphan-unlink loop was deleting the
/// newly-placed file (wrong). Guard: orphan paths that match a
/// destination written during this apply should skip the unlink.
#[test]
fn apply_skips_orphan_unlink_when_path_was_just_occupied_by_move() {
    let (_tmp, src, music, conn) = fresh_world();

    // The new imported file (in inbox) — organize will move this.
    let inbox_file = src.join("03 Song.mp3");
    let track_id = add_album_track(
        &conn,
        inbox_file.clone(),
        "Artist",
        "Album",
        3,
        "Song",
        Some(2024),
    );

    // Destination the template resolves to.
    let dest = music.join("Artist/Album (2024)/03 Song.mp3");

    // Seed an orphan row at exactly that destination (simulates the
    // dup-replace: old track row deleted, file path logged for cleanup).
    // Also put a real file there so the move's rename has something to
    // overwrite — this mirrors the real-world race.
    touch(&dest);
    queries::insert_orphan(
        &conn,
        &music,
        &dest.display().to_string(),
        Some("Song"),
        Some("Artist"),
        Some("Album"),
        "replaced by duplicate during import",
    )
    .unwrap();

    let plan = plan_organize(&conn, &settings_with_music_dir(&music), OrganizeFilter::All).unwrap();
    assert_eq!(plan.moves.len(), 1, "expected one move for the new import");
    assert_eq!(plan.file_orphans.len(), 1, "expected one pending orphan");
    assert_eq!(plan.moves[0].to, dest);

    let result = apply_organize(&conn, &music, &plan, "move", &[music.clone()]).unwrap();

    assert_eq!(result.moved, 1);
    assert_eq!(result.file_orphans_removed, 1);
    assert!(
        result.errors.is_empty(),
        "no errors expected, got: {:?}",
        result.errors
    );
    // THE key assertion: the freshly-moved file must survive. Before the
    // fix, the orphan-unlink loop deleted this file.
    assert!(
        dest.exists(),
        "newly-moved file was deleted by the orphan-unlink loop — bug regressed"
    );
    // DB path resolves to the new location (via the queries layer, since
    // the row stores the relative-to-music_dir form now).
    let row = queries::get_track(&conn, &music, track_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.file_path, dest.display().to_string());
    // Orphan tracking row is cleared (the move resolved it).
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM orphaned_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 0);
}

// ── plan_delete_collection / apply_delete_collection ─────────────

/// Build a track sitting at a real on-disk path under `music`, in the named
/// collection, with `collection_file_path` set to the same path.
fn seed_collection_file(
    conn: &Connection,
    music: &Path,
    coll: &str,
    rel_path: &str,
    artist: &str,
    title: &str,
) -> (i64, i64, PathBuf) {
    let abs = music.join(rel_path);
    let tid = add_loose_track(conn, abs.clone(), artist, title);
    let coll_id = ensure_collection(conn, coll, tid);
    queries::update_collection_track_path(conn, &music, coll_id, tid, &abs.display().to_string()).unwrap();
    (tid, coll_id, abs)
}

#[test]
fn plan_delete_classifies_files_inside_music_dir_for_deletion() {
    let (_tmp, _src, music, conn) = fresh_world();
    let (_tid, coll_id, abs) = seed_collection_file(
        &conn,
        &music,
        "Mix",
        "Collections/Mix/song.mp3",
        "Artist",
        "Song",
    );

    let plan = plan_delete_collection(&conn, coll_id, &music).unwrap();

    assert_eq!(plan.files_to_delete, vec![abs]);
    assert!(plan.files_outside_music_dir.is_empty());
}

#[test]
fn plan_delete_classifies_files_outside_music_dir_as_skipped() {
    let (tmp, _src, music, conn) = fresh_world();
    // collection file lives outside music_dir
    let outside = tmp.path().join("elsewhere/song.mp3");
    let tid = add_loose_track(&conn, outside.clone(), "Artist", "Song");
    let coll_id = ensure_collection(&conn, "Mix", tid);
    queries::update_collection_track_path(&conn, &music, coll_id, tid, &outside.display().to_string())
        .unwrap();

    let plan = plan_delete_collection(&conn, coll_id, &music).unwrap();

    assert!(plan.files_to_delete.is_empty());
    assert_eq!(plan.files_outside_music_dir, vec![outside]);
}

#[test]
fn plan_delete_marks_track_orphaned_when_no_album_and_no_other_collection() {
    let (_tmp, _src, music, conn) = fresh_world();
    let (tid, coll_id, _) = seed_collection_file(
        &conn,
        &music,
        "Mix",
        "Collections/Mix/song.mp3",
        "Artist",
        "Song",
    );

    let plan = plan_delete_collection(&conn, coll_id, &music).unwrap();

    assert_eq!(plan.orphaned_track_ids, vec![tid]);
}

#[test]
fn plan_delete_promotes_alternate_collection_path_when_track_has_other_home() {
    let (_tmp, _src, music, conn) = fresh_world();
    // Loose track in two collections; tracks.file_path matches A's copy.
    let abs_a = music.join("Collections/A/song.mp3");
    let tid = add_loose_track(&conn, abs_a.clone(), "Artist", "Song");
    let (coll_a, _) = queries::get_or_create_collection(&conn, "A").unwrap();
    let (coll_b, _) = queries::get_or_create_collection(&conn, "B").unwrap();
    queries::add_track_to_collection(&conn, coll_a, tid).unwrap();
    queries::add_track_to_collection(&conn, coll_b, tid).unwrap();
    queries::update_collection_track_path(&conn, &music, coll_a, tid, &abs_a.display().to_string())
        .unwrap();
    let abs_b = music.join("Collections/B/song.mp3");
    queries::update_collection_track_path(&conn, &music, coll_b, tid, &abs_b.display().to_string())
        .unwrap();

    let plan = plan_delete_collection(&conn, coll_a, &music).unwrap();

    assert!(
        plan.orphaned_track_ids.is_empty(),
        "track has another home, not orphaned"
    );
    assert_eq!(
        plan.promote_paths,
        vec![(tid, abs_b.display().to_string())],
        "should promote B's path to be the new tracks.file_path",
    );
}

#[test]
fn apply_delete_with_files_true_removes_files_and_orphan_track_rows() {
    let (_tmp, _src, music, conn) = fresh_world();
    let (tid, coll_id, abs) = seed_collection_file(
        &conn,
        &music,
        "Mix",
        "Collections/Mix/song.mp3",
        "Artist",
        "Song",
    );
    let plan = plan_delete_collection(&conn, coll_id, &music).unwrap();

    let result = apply_delete_collection(&conn, &music, &plan, true).unwrap();

    assert_eq!(result.files_deleted, 1);
    assert_eq!(result.tracks_orphaned_removed, 1);
    assert!(!abs.exists(), "physical file should be deleted");
    let track_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(track_count, 0, "orphaned track row should be deleted");
    let coll_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM collections WHERE id = ?1",
            [coll_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(coll_count, 0, "collection row should be gone");
}

#[test]
fn apply_delete_with_files_false_keeps_files_but_cleans_db() {
    let (_tmp, _src, music, conn) = fresh_world();
    let (tid, coll_id, abs) = seed_collection_file(
        &conn,
        &music,
        "Mix",
        "Collections/Mix/song.mp3",
        "Artist",
        "Song",
    );
    let plan = plan_delete_collection(&conn, coll_id, &music).unwrap();

    let result = apply_delete_collection(&conn, &music, &plan, false).unwrap();

    assert_eq!(
        result.files_deleted, 0,
        "files=false should leave files alone"
    );
    assert_eq!(
        result.tracks_orphaned_removed, 1,
        "orphan tracks should be removed from the library even when files remain"
    );
    assert!(abs.exists(), "physical file should remain");
    // Collection still gone
    let coll_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM collections WHERE id = ?1",
            [coll_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(coll_count, 0);
    // Track row is gone, so deleting a collection does not silently create loose tracks.
    let track_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(track_count, 0);
}

// ── remove_empty_parents safety floor ────────────────────────────

#[test]
fn remove_empty_parents_refuses_to_touch_path_outside_roots() {
    let tmp = TempDir::new().unwrap();
    let unmanaged = tmp.path().join("unmanaged/deep");
    std::fs::create_dir_all(&unmanaged).unwrap();

    // The unmanaged dir is NOT under any declared root — function must do nothing.
    let fake_root = tmp.path().join("some_other_root");
    let cleaned = remove_empty_parents(&unmanaged, &[fake_root]);

    assert_eq!(cleaned, 0);
    assert!(
        unmanaged.exists(),
        "dir outside any root must not be deleted"
    );
}

#[test]
fn remove_empty_parents_never_deletes_root_itself() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("music");
    let child = root.join("Artist/Album");
    std::fs::create_dir_all(&child).unwrap();

    let cleaned = remove_empty_parents(&child, &[root.clone()]);

    // Both Album and Artist are inside the root and empty → cleaned.
    // But the root itself must survive even though it is empty.
    assert_eq!(cleaned, 2);
    assert!(root.exists(), "root must never be deleted");
    assert!(!root.join("Artist").exists());
}

#[test]
fn remove_empty_parents_stops_at_root_boundary() {
    // Climbing must not cross a root boundary even if ancestors happen to
    // be empty on disk.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("music");
    let sibling = tmp.path().join("sibling");
    let child = root.join("Artist");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();

    let _ = remove_empty_parents(&child, &[root.clone()]);

    assert!(!child.exists(), "empty child inside root should be cleaned");
    assert!(root.exists(), "root preserved");
    assert!(sibling.exists(), "sibling outside root must be untouched");
    assert!(tmp.path().exists(), "walk must not climb above root");
}

// Deletion (DeletePlan/apply_delete_plan) tests live in `pruner_tests.rs`.
