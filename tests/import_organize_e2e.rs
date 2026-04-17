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
        eprintln!("Skipping: fixture {} missing", fixture.display());
        return;
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
    let result = importer::import(&conn, &inbox, /*loose*/ false, /*pretend*/ false, None)
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
        &plan,
        "move",
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

    // Step 4: DB path reflects the new location.
    let db_path: String = conn
        .query_row("SELECT file_path FROM tracks LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        PathBuf::from(&db_path),
        planned_target,
        "tracks.file_path should match the move target"
    );

    // Step 5: no lingering unresolved paths.
    let missing = kyoku::core::relocator::verify_paths(&conn).unwrap();
    assert!(
        missing.is_empty(),
        "no DB paths should point to missing files after organize, got {:?}",
        missing
    );
}
