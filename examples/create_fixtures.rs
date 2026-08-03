/// Helper to create test fixture audio files.
/// Run with: cargo run --example create_fixtures
use std::fs;
use std::io::Write;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_library");
    fs::create_dir_all(&dir).unwrap();

    // Create a minimal valid MP3 file with ID3v2 tags
    create_tagged_mp3(
        &dir.join("tagged.mp3"),
        "Test Title",
        "Test Artist",
        "Test Album",
        2024,
        1,
    );

    // Create an MP3 with no title tag
    create_mp3_no_title(&dir.join("no_title.mp3"));

    // Create a non-audio file
    fs::write(dir.join("not_audio.txt"), "this is not audio").unwrap();

    // Create an MP3 with CJK tags
    create_tagged_mp3(
        &dir.join("cjk_tagged.mp3"),
        "少女綺想曲",
        "東方Project",
        "東方紅魔郷",
        2002,
        3,
    );

    println!("Fixtures created in {}", dir.display());
}

fn create_tagged_mp3(path: &Path, title: &str, artist: &str, album: &str, year: u32, track: u32) {
    let mut file = fs::File::create(path).unwrap();
    write_valid_mp3_frames(&mut file);
    drop(file);

    use lofty::config::WriteOptions;
    use lofty::prelude::*;
    use lofty::tag::{Accessor, Tag, TagType};

    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_string());
    tag.set_artist(artist.to_string());
    tag.set_album(album.to_string());
    tag.insert(lofty::tag::TagItem::new(
        lofty::tag::ItemKey::Year,
        lofty::tag::ItemValue::Text(year.to_string()),
    ));
    tag.set_track(track);

    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

fn create_mp3_no_title(path: &Path) {
    let mut file = fs::File::create(path).unwrap();
    write_valid_mp3_frames(&mut file);
    drop(file);

    use lofty::config::WriteOptions;
    use lofty::prelude::*;
    use lofty::tag::{Accessor, Tag, TagType};

    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_artist("Some Artist".to_string());

    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

/// Write multiple valid MPEG1 Layer 3 frames to make a valid MP3 file.
///
/// Frame header (4 bytes):
///   Byte 0: 0xFF (sync)
///   Byte 1: 0xFB = 11111011
///     - sync bits [7:5] = 111
///     - MPEG version [4:3] = 11 (MPEG1)
///     - Layer [2:1] = 01 (Layer 3)
///     - Protection [0] = 1 (no CRC)
///   Byte 2: 0x90 = 10010000
///     - Bitrate [7:4] = 1001 (128kbps for MPEG1 Layer3)
///     - Sample rate [3:2] = 00 (44100Hz for MPEG1)
///     - Padding [1] = 0 (no padding)
///     - Private [0] = 0
///   Byte 3: 0x00 = 00000000
///     - Channel mode [7:6] = 00 (stereo)
///     - Mode extension [5:4] = 00
///     - Copyright [3] = 0
///     - Original [2] = 0
///     - Emphasis [1:0] = 00 (none)
///
/// Frame size = floor(144 * bitrate / sample_rate) = floor(144 * 128000 / 44100) = 417 bytes
fn write_valid_mp3_frames(file: &mut fs::File) {
    let frame_header: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];
    let frame_size = 417; // bytes per frame at 128kbps / 44100Hz

    // Write 3 frames to ensure lofty considers this a valid MP3
    for _ in 0..3 {
        let mut frame = vec![0u8; frame_size];
        frame[..4].copy_from_slice(&frame_header);
        // Bytes 5-35: side information (32 bytes for stereo MPEG1 Layer3)
        // Leave as zeros — valid "silence" side info
        file.write_all(&frame).unwrap();
    }
}
