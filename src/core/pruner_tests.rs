use super::*;
use crate::db;
use crate::db::models::{AudioFormat, TagStatus, Track};
use crate::db::queries;
use rusqlite::Connection;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────

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
    queries::insert_track(conn, std::path::Path::new(""), &track, Some(album_id), None).unwrap()
}

fn add_loose_track(conn: &Connection, src_path: PathBuf, artist: &str, title: &str) -> i64 {
    touch(&src_path);
    let track = make_track_struct(src_path, None, title, artist, 1);
    queries::insert_track(conn, std::path::Path::new(""), &track, None, None).unwrap()
}

/// (TempDir, src_dir, music_dir, conn).
fn fresh_world() -> (TempDir, PathBuf, PathBuf, Connection) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("inbox");
    let music = tmp.path().join("music");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&music).unwrap();
    let conn = db::open_memory().unwrap();
    (tmp, src, music, conn)
}

// ── Tests ────────────────────────────────────────────────────────

#[test]
fn plan_delete_tracks_collects_collection_copies() {
    let (_tmp, _src, music, conn) = fresh_world();
    // Primary track file under music_dir.
    let primary = music.join("Artist/Album/track.mp3");
    let tid = add_album_track(&conn, primary.clone(), "Artist", "Album", 1, "Track", None);
    // Add the track to two collections, each with its own file copy path.
    let (coll_a, _) = queries::get_or_create_collection(&conn, "A").unwrap();
    queries::add_track_to_collection(&conn, coll_a, tid).unwrap();
    let copy_a = music.join("Collections/A/track.mp3");
    queries::update_collection_track_path(
        &conn,
        &music,
        coll_a,
        tid,
        &copy_a.display().to_string(),
    )
    .unwrap();
    let (coll_b, _) = queries::get_or_create_collection(&conn, "B").unwrap();
    queries::add_track_to_collection(&conn, coll_b, tid).unwrap();
    let copy_b = music.join("Collections/B/track.mp3");
    queries::update_collection_track_path(
        &conn,
        &music,
        coll_b,
        tid,
        &copy_b.display().to_string(),
    )
    .unwrap();

    let plan = plan_delete_tracks(&conn, &music, &[tid], std::slice::from_ref(&music)).unwrap();

    assert_eq!(plan.track_ids, vec![tid]);
    assert!(
        plan.album_ids.is_empty(),
        "plan_delete_tracks never removes albums"
    );
    assert_eq!(plan.files_to_delete, vec![primary]);
    let mut copies = plan.collection_copies_to_delete.clone();
    copies.sort();
    let mut expected = vec![copy_a, copy_b];
    expected.sort();
    assert_eq!(copies, expected);
}

#[test]
fn plan_delete_album_lists_tracks_and_album_row() {
    let (_tmp, _src, music, conn) = fresh_world();
    let p1 = music.join("Artist/Album/01.mp3");
    let p2 = music.join("Artist/Album/02.mp3");
    let t1 = add_album_track(&conn, p1.clone(), "Artist", "Album", 1, "One", None);
    let t2 = add_album_track(&conn, p2.clone(), "Artist", "Album", 2, "Two", None);
    // Resolve the album_id shared by these tracks.
    let aid: i64 = conn
        .query_row("SELECT album_id FROM tracks WHERE id = ?1", [t1], |r| {
            r.get(0)
        })
        .unwrap();

    let plan = plan_delete_albums(&conn, &music, &[aid], std::slice::from_ref(&music)).unwrap();

    let mut track_ids = plan.track_ids.clone();
    track_ids.sort();
    let mut expected_tids = vec![t1, t2];
    expected_tids.sort();
    assert_eq!(track_ids, expected_tids);
    assert_eq!(plan.album_ids, vec![aid]);
    let mut files = plan.files_to_delete.clone();
    files.sort();
    let mut expected_files = vec![p1, p2];
    expected_files.sort();
    assert_eq!(files, expected_files);
}

#[test]
fn collection_only_tracks_are_not_loose() {
    let (_tmp, _src, music, conn) = fresh_world();
    let p = music.join("Collections/Mix/song.mp3");
    let tid = add_loose_track(&conn, p.clone(), "Artist", "Song");
    assert_eq!(queries::count_loose_tracks(&conn).unwrap(), 1);

    let (coll_id, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();
    queries::add_track_to_collection(&conn, coll_id, tid).unwrap();

    assert_eq!(queries::count_loose_tracks(&conn).unwrap(), 0);
    assert!(queries::list_loose_track_ids(&conn).unwrap().is_empty());
}

#[test]
fn delete_album_preserves_tracks_that_belong_to_collections() {
    let (_tmp, _src, music, conn) = fresh_world();
    let primary = music.join("Artist/Album/song.mp3");
    let tid = add_album_track(&conn, primary.clone(), "Artist", "Album", 1, "Song", None);
    let aid: i64 = conn
        .query_row("SELECT album_id FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    let (coll_id, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();
    queries::add_track_to_collection(&conn, coll_id, tid).unwrap();
    let copy = music.join("Collections/Mix/song.mp3");
    touch(&copy);
    queries::update_collection_track_path(&conn, &music, coll_id, tid, &copy.display().to_string())
        .unwrap();

    let plan = plan_delete_albums(&conn, &music, &[aid], std::slice::from_ref(&music)).unwrap();
    assert!(
        plan.track_ids.is_empty(),
        "collection tracks should survive album deletion"
    );
    assert_eq!(plan.album_survivor_track_ids, vec![tid]);
    assert_eq!(plan.promote_paths, vec![(tid, copy.display().to_string())]);
    assert_eq!(plan.files_to_delete, vec![primary.clone()]);

    let report = apply_delete_plan(&conn, &music, &plan, true, std::slice::from_ref(&music)).unwrap();

    assert_eq!(report.albums_deleted, 1);
    assert_eq!(report.tracks_deleted, 0);
    assert_eq!(report.files_deleted, 1);
    assert!(!primary.exists());
    let album_id: Option<i64> = conn
        .query_row("SELECT album_id FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(album_id, None);
    // tracks.file_path now stores the relative form — read through the
    // queries layer to resolve it back to absolute for the assertion.
    let row = queries::get_track(&conn, &music, tid).unwrap().unwrap();
    assert_eq!(row.file_path, copy.display().to_string());
    let membership_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM collection_tracks WHERE collection_id = ?1 AND track_id = ?2",
            rusqlite::params![coll_id, tid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(membership_count, 1);
}

#[test]
fn delete_album_without_file_delete_does_not_promote_collection_copy() {
    let (_tmp, _src, music, conn) = fresh_world();
    let primary = music.join("Artist/Album/song.mp3");
    let tid = add_album_track(&conn, primary.clone(), "Artist", "Album", 1, "Song", None);
    let aid: i64 = conn
        .query_row("SELECT album_id FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    let (coll_id, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();
    queries::add_track_to_collection(&conn, coll_id, tid).unwrap();
    let copy = music.join("Collections/Mix/song.mp3");
    touch(&copy);
    queries::update_collection_track_path(&conn, &music, coll_id, tid, &copy.display().to_string())
        .unwrap();

    let plan = plan_delete_albums(&conn, &music, &[aid], std::slice::from_ref(&music)).unwrap();
    assert_eq!(plan.promote_paths, vec![(tid, copy.display().to_string())]);

    let report = apply_delete_plan(&conn, &music, &plan, false, std::slice::from_ref(&music)).unwrap();

    assert_eq!(report.files_deleted, 0);
    let row = queries::get_track(&conn, &music, tid).unwrap().unwrap();
    assert_eq!(row.file_path, primary.display().to_string());
    assert!(primary.exists());
    assert!(copy.exists());
}

#[test]
fn plan_delete_tracks_dedupes_primary_equal_to_collection_copy() {
    let (_tmp, _src, music, conn) = fresh_world();
    let primary = music.join("Collections/Mix/song.mp3");
    let tid = add_loose_track(&conn, primary.clone(), "Artist", "Song");
    let (coll_id, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();
    queries::add_track_to_collection(&conn, coll_id, tid).unwrap();
    queries::update_collection_track_path(
        &conn,
        &music,
        coll_id,
        tid,
        &primary.display().to_string(),
    )
    .unwrap();

    let plan = plan_delete_tracks(&conn, &music, &[tid], std::slice::from_ref(&music)).unwrap();

    assert_eq!(plan.files_to_delete, vec![primary]);
    assert!(plan.collection_copies_to_delete.is_empty());
    assert_eq!(plan.deletable_file_count(), 1);
}

#[test]
fn delete_album_keeps_primary_file_for_collection_track_without_copy() {
    let (_tmp, _src, music, conn) = fresh_world();
    let primary = music.join("Artist/Album/song.mp3");
    let tid = add_album_track(&conn, primary.clone(), "Artist", "Album", 1, "Song", None);
    let aid: i64 = conn
        .query_row("SELECT album_id FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    let (coll_id, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();
    queries::add_track_to_collection(&conn, coll_id, tid).unwrap();

    let plan = plan_delete_albums(&conn, &music, &[aid], std::slice::from_ref(&music)).unwrap();
    assert!(plan.track_ids.is_empty());
    assert_eq!(plan.album_survivor_track_ids, vec![tid]);
    assert!(plan.promote_paths.is_empty());
    assert!(
        plan.files_to_delete.is_empty(),
        "do not delete a surviving collection track's only physical file"
    );

    let report =
        apply_delete_plan(&conn, &music, &plan, true, std::slice::from_ref(&music)).unwrap();

    assert_eq!(report.albums_deleted, 1);
    assert_eq!(report.files_deleted, 0);
    assert!(primary.exists());
    let (album_id, file_path): (Option<i64>, String) = conn
        .query_row(
            "SELECT album_id, file_path FROM tracks WHERE id = ?1",
            [tid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(album_id, None);
    assert_eq!(file_path, primary.display().to_string());
}

#[test]
fn apply_delete_plan_keeps_files_when_flag_false() {
    let (_tmp, _src, music, conn) = fresh_world();
    let p = music.join("Artist/Album/song.mp3");
    let tid = add_album_track(&conn, p.clone(), "Artist", "Album", 1, "Song", None);
    let plan = plan_delete_tracks(&conn, &music, &[tid], std::slice::from_ref(&music)).unwrap();

    let report = apply_delete_plan(&conn, &music, &plan, false, std::slice::from_ref(&music)).unwrap();

    assert_eq!(report.tracks_deleted, 1);
    assert_eq!(
        report.files_deleted, 0,
        "delete_files=false leaves physical files alone"
    );
    assert!(p.exists(), "file should survive");
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks WHERE id = ?1", [tid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0, "track row should still be removed");
}

#[test]
fn apply_delete_plan_removes_files_and_empty_parents() {
    let (_tmp, _src, music, conn) = fresh_world();
    let p = music.join("Artist/Album/song.mp3");
    let tid = add_album_track(&conn, p.clone(), "Artist", "Album", 1, "Song", None);
    let plan = plan_delete_tracks(&conn, &music, &[tid], std::slice::from_ref(&music)).unwrap();

    let report = apply_delete_plan(&conn, &music, &plan, true, std::slice::from_ref(&music)).unwrap();

    assert_eq!(report.files_deleted, 1);
    assert_eq!(report.tracks_deleted, 1);
    assert!(!p.exists(), "file should be deleted");
    // Empty album + artist dirs swept up; music_dir itself preserved.
    assert!(
        !music.join("Artist/Album").exists(),
        "empty album dir cleaned"
    );
    assert!(!music.join("Artist").exists(), "empty artist dir cleaned");
    assert!(music.exists(), "music_dir root preserved");
    assert!(report.dirs_cleaned >= 2);
}

#[test]
fn apply_delete_plan_rejects_path_outside_managed_roots() {
    // A plan built against a different root should still refuse to touch
    // files outside the cleanup_roots passed at apply time.
    let (tmp, _src, music, conn) = fresh_world();
    let outside = tmp.path().join("elsewhere/song.mp3");
    let tid = add_loose_track(&conn, outside.clone(), "Artist", "Song");
    // Plan treats the *actual* file location as managed so it lands in
    // `files_to_delete` — but at apply time we pass a different roots list.
    let plan = plan_delete_tracks(&conn, &music, &[tid], &[tmp.path().to_path_buf()]).unwrap();
    assert_eq!(plan.files_to_delete, vec![outside.clone()]);

    let report = apply_delete_plan(&conn, &music, &plan, true, std::slice::from_ref(&music)).unwrap();

    assert_eq!(report.files_deleted, 0, "apply must refuse unmanaged paths");
    assert!(outside.exists(), "file outside cleanup_roots is untouched");
    // Track row still cleaned from the DB (that's a pure DB op).
    assert_eq!(report.tracks_deleted, 1);
}

#[test]
fn plan_delete_flags_files_outside_managed_roots() {
    let (tmp, _src, music, conn) = fresh_world();
    let outside = tmp.path().join("elsewhere/song.mp3");
    let tid = add_loose_track(&conn, outside.clone(), "Artist", "Song");

    // managed_roots contains only music_dir; outside path lands in
    // `files_outside_managed` and must not appear in `files_to_delete`.
    let plan = plan_delete_tracks(&conn, &music, &[tid], std::slice::from_ref(&music)).unwrap();

    assert!(plan.files_to_delete.is_empty());
    assert_eq!(plan.files_outside_managed, vec![outside]);
}
