//! Tag dump debug utility.
//!
//! **Manual test — ignored by default.** Prints the tags kyoku sees for
//! every audio file passed via the `DUMP_DIR` env var (scanned
//! non-recursively). Used to verify what metadata Soulseek rips actually
//! carry when debugging MB match behaviour.
//!
//! ```sh
//! DUMP_DIR="/path/to/album folder" cargo test --test tag_dump -- --ignored --nocapture
//! ```

use std::path::PathBuf;

#[test]
#[ignore = "debug utility: set DUMP_DIR and run manually"]
fn dump_tags_under_dir() {
    let dir = std::env::var("DUMP_DIR").expect("set DUMP_DIR to a folder");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["flac", "mp3", "ogg", "m4a", "opus", "wav"].contains(&e))
        })
        .collect();
    files.sort();

    for f in &files {
        match kyoku::core::tagger::read_track_with_tags(f) {
            Ok((track, tags)) => {
                println!("{}", f.file_name().unwrap().to_string_lossy());
                println!(
                    "  title={:?} artist={:?} album={:?} album_artist={:?}",
                    tags.title, tags.artist, tags.album, tags.album_artist
                );
                println!(
                    "  year={:?} track_no={:?} disc={:?} dur={:?}mb_release_id={:?}",
                    tags.year,
                    tags.track_number,
                    tags.disc_number,
                    tags.duration.map(|d| d.as_millis()),
                    tags.mb_release_id
                );
                println!(
                    "  track.title={:?} track.artist={:?} genre={:?}",
                    track.title, track.artist, tags.genre
                );
            }
            Err(e) => println!("{}: READ ERROR: {}", f.display(), e),
        }
    }
}