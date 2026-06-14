//! End-to-end smoke test: import real audio → organize → verify FS + DB.
//!
//! Unlike the unit tests, this one exercises the real tag-reading path by
//! copying a real fixture MP3 into a temp inbox, running `importer::import`,
//! then `organizer::plan_organize` + `apply_organize`, and asserting that
//! the file lands at the template-rendered path under `music/` and that
//! the DB reflects the new location.

use std::path::PathBuf;

use kyoku::config::Settings;
use kyoku::core::{importer, organizer};
use kyoku::db;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_library")
}

#[test]
fn e2e_import_album_then_organize_lands_files_under_music_dir() {
    let fixture = fixtures_dir().join("tagged.mp3");
    if !fixture.exists() {
        panic!(
            "fixture missing: {} — see tests/fixtures/README",
            fixture.display()
        );
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let inbox = tmp.path().join("inbox");
    let music = tmp.path().join("music");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::create_dir_all(&music).unwrap();

    // Copy real fixture bytes into the inbox.
    let inbox_file = inbox.join("tagged.mp3");
    std::fs::copy(&fixture, &inbox_file).unwrap();

    let conn = db::open_memory().unwrap();

    // Step 1: import from inbox.
    let result = importer::import(
        &conn, &music, &inbox, /*loose*/ false, /*pretend*/ false, None,
    )
    .expect("import should succeed");
    assert_eq!(result.imported, 1, "exactly one track should be imported");
    assert_eq!(result.skipped_error, 0);

    // Step 2: organize into music_dir using default templates.
    let mut settings = Settings::default();
    settings.library.music_dir = music.clone();

    let plan = organizer::plan_organize(&conn, &settings, organizer::OrganizeFilter::All)
        .expect("plan_organize should succeed");
    assert_eq!(plan.moves.len(), 1, "exactly one move should be planned");
    assert!(plan.copies.is_empty());
    assert_eq!(plan.missing_sources.len(), 0);

    let planned_target = plan.moves[0].to.clone();
    assert!(
        planned_target.starts_with(&music),
        "planned target {:?} must live under music_dir {:?}",
        planned_target,
        music
    );

    let result = organizer::apply_organize(
        &conn,
        &music,
        &plan,
        kyoku::config::OrganizeOperation::Move,
        &[music.clone(), inbox.clone()],
    )
    .expect("apply_organize should succeed");
    assert_eq!(result.moved, 1);
    assert_eq!(result.copied, 0);
    assert!(
        result.errors.is_empty(),
        "apply should report no errors, got {:?}",
        result.errors
    );

    // Step 3: filesystem assertions.
    assert!(
        planned_target.exists(),
        "target file {:?} should exist after apply",
        planned_target
    );
    assert!(
        !inbox_file.exists(),
        "source file {:?} should be gone after move",
        inbox_file
    );

    // Step 4: DB row resolves to the new location. The stored path is now
    // relative to music_dir (the v7 storage convention), and the queries
    // layer rejoins it on read — fetch a TrackRow via the public API to
    // verify the resolved path matches the move target.
    let track = kyoku::db::queries::get_album_tracks(&conn, &music, /*album_id*/ 1)
        .unwrap()
        .into_iter()
        .next()
        .expect("at least one track row");
    assert_eq!(
        PathBuf::from(&track.file_path),
        planned_target,
        "resolved file_path should match the move target"
    );

    // Step 5: renaming music_dir is the central use case of the refactor —
    // the same DB resolves under a different prefix without any rewrite.
    let renamed = tmp.path().join("music_renamed");
    std::fs::rename(&music, &renamed).unwrap();
    let renamed_target = renamed.join(planned_target.strip_prefix(&music).unwrap());
    let track2 = kyoku::db::queries::get_album_tracks(&conn, &renamed, 1)
        .unwrap()
        .into_iter()
        .next()
        .expect("track still listed after music_dir rename");
    assert_eq!(
        PathBuf::from(&track2.file_path),
        renamed_target,
        "file_path resolves under the renamed music_dir without any DB rewrite"
    );
    assert!(
        renamed_target.exists(),
        "renamed target {:?} should exist on disk",
        renamed_target
    );
}
