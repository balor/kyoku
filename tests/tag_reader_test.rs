use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_library")
}

#[test]
fn test_read_mp3_tags() {
    let path = fixtures_dir().join("tagged.mp3");
    if !path.exists() {
        eprintln!(
            "Skipping test: fixture file not found at {}",
            path.display()
        );
        return;
    }

    let track = kyoku::core::tagger::read_track(&path).unwrap();
    assert!(!track.title.is_empty());
    assert_eq!(track.file_format, kyoku::db::models::AudioFormat::Mp3);
    assert_eq!(track.tag_status, kyoku::db::models::TagStatus::Unmatched);
}

#[test]
fn test_read_flac_tags() {
    let path = fixtures_dir().join("tagged.flac");
    if !path.exists() {
        eprintln!(
            "Skipping test: fixture file not found at {}",
            path.display()
        );
        return;
    }

    let track = kyoku::core::tagger::read_track(&path).unwrap();
    assert!(!track.title.is_empty());
    assert_eq!(track.file_format, kyoku::db::models::AudioFormat::Flac);
}

#[test]
fn test_read_ogg_tags() {
    let path = fixtures_dir().join("tagged.ogg");
    if !path.exists() {
        eprintln!(
            "Skipping test: fixture file not found at {}",
            path.display()
        );
        return;
    }

    let track = kyoku::core::tagger::read_track(&path).unwrap();
    assert!(!track.title.is_empty());
    assert_eq!(track.file_format, kyoku::db::models::AudioFormat::Ogg);
}

#[test]
fn test_missing_title_derives_from_filename() {
    let path = fixtures_dir().join("no_title.mp3");
    if !path.exists() {
        eprintln!(
            "Skipping test: fixture file not found at {}",
            path.display()
        );
        return;
    }

    let track = kyoku::core::tagger::read_track(&path).unwrap();
    assert_eq!(track.title, "no_title");
}

#[test]
fn test_unsupported_format() {
    let path = fixtures_dir().join("not_audio.txt");
    if !path.exists() {
        eprintln!(
            "Skipping test: fixture file not found at {}",
            path.display()
        );
        return;
    }

    let result = kyoku::core::tagger::read_track(&path);
    assert!(result.is_err());
}
