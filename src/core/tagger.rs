//! Audio-file tag read/write.
//!
//! The read side (`read_tags`, `read_track`) maps lofty's format-agnostic
//! `Tag` accessors to our domain types. `read_all_frames` goes further and
//! enumerates every standard-keyed item so the tag editor can present the
//! whole frame list grouped by kind.
//!
//! The write side (`write_tags`) is the inverse: apply a `TagChanges`
//! delta to the file's primary tag, then replace the file atomically via
//! a copy-tmp-rename dance. Lofty itself rewrites in place, which is
//! fine for our small tags but leaves a window where an interrupted
//! write could truncate the file — the rename guards against that.
//!
//! Scope: standard `ItemKey` variants only. Custom frames (ID3v2 TXXX,
//! Vorbis freeform fields that don't map to a known `ItemKey`) are out
//! of scope for this editor; the common case is title/artist/album/etc.,
//! and the MusicBrainz and ReplayGain keys we *do* care about all have
//! dedicated `ItemKey` variants.
//!
//! Known limitation: writing `MusicBrainz*` keys on MP3/ID3v2 is lossy
//! through lofty's generic `Tag` API — the item is accepted but dropped
//! during ID3v2 serialization (it needs to be written as a TXXX frame
//! via the ID3v2-specific API). Reads work fine. Vorbis/APE/MP4
//! containers round-trip MB IDs correctly via the generic path. This
//! covers every real-world editing case except "edit an MB ID on an
//! MP3" — uncommon since we write those during MB matching, not by
//! hand.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag, TagItem};

use crate::db::models::{AudioFormat, TagStatus, Track};
use crate::error::{KyokuError, Result};

/// Metadata read from a single audio file.
#[derive(Debug, Clone)]
pub struct TagData {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub genre: Option<String>,
    pub duration: Option<Duration>,
}

/// Read tags and audio properties from a file.
pub fn read_tags(path: impl AsRef<Path>) -> Result<TagData> {
    let path = path.as_ref();

    let tagged_file = lofty::read_from_path(path).map_err(|e| KyokuError::TagRead {
        path: path.to_path_buf(),
        source: e,
    })?;

    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    let properties = tagged_file.properties();

    let data = TagData {
        title: tag.and_then(|t| t.title().map(|s| s.to_string())),
        artist: tag.and_then(|t| t.artist().map(|s| s.to_string())),
        album: tag.and_then(|t| t.album().map(|s| s.to_string())),
        album_artist: tag.and_then(|t| {
            // lofty doesn't have a direct album_artist accessor on Accessor trait,
            // so we check common tag items
            t.get_string(ItemKey::AlbumArtist).map(|s| s.to_string())
        }),
        year: tag.and_then(|t| {
            // year() was removed in lofty 0.23, use get_string with Year key
            t.get_string(ItemKey::Year)
                .and_then(|s| s.parse::<u32>().ok())
        }),
        track_number: tag.and_then(|t| t.track()),
        disc_number: tag.and_then(|t| t.disk()),
        genre: tag.and_then(|t| t.genre().map(|s| s.to_string())),
        duration: Some(properties.duration()),
    };

    Ok(data)
}

/// Read a file's tags and audio properties into a Track struct.
/// The track is not yet associated with an album (album_id = None).
pub fn read_track(path: impl AsRef<Path>) -> Result<Track> {
    let path = path.as_ref();

    if !path.exists() {
        return Err(KyokuError::FileNotFound {
            path: path.to_path_buf(),
        });
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let format = AudioFormat::from_extension(ext);

    if !format.is_supported() {
        return Err(KyokuError::UnsupportedFormat {
            ext: ext.to_string(),
        });
    }

    let tag_data = read_tags(path)?;

    let tagged_file = lofty::read_from_path(path).map_err(|e| KyokuError::TagRead {
        path: path.to_path_buf(),
        source: e,
    })?;
    let properties = tagged_file.properties();

    // If title is missing, derive from filename (spec rule #2)
    let title = tag_data.title.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    });

    let track = Track {
        id: None,
        album_id: None,
        title,
        artist: tag_data.artist,
        track_number: tag_data.track_number,
        disc_number: tag_data.disc_number.unwrap_or(1),
        duration_ms: tag_data.duration.map(|d| d.as_millis() as u64),
        mbid: None,
        file_path: path.to_path_buf(),
        file_format: format,
        bitrate: properties.audio_bitrate(),
        sample_rate: properties.sample_rate(),
        tag_status: TagStatus::Unmatched,
        source_dir: path.parent().map(|p| p.to_path_buf()),
    };

    Ok(track)
}

// ---------- Frame read-all ----------

/// A single tag frame as surfaced to the tag editor.
#[derive(Debug, Clone)]
pub struct FrameEntry {
    pub group: FrameGroup,
    pub key: ItemKey,
    /// Human-readable label (e.g. "Title", "MusicBrainz Release Id").
    pub display_name: String,
    /// One or more values — multi-value frames are preserved as-is.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameGroup {
    Standard,
    MusicBrainz,
    ReplayGain,
}

impl FrameGroup {
    pub fn label(self) -> &'static str {
        match self {
            FrameGroup::Standard => "Standard",
            FrameGroup::MusicBrainz => "MusicBrainz",
            FrameGroup::ReplayGain => "ReplayGain",
        }
    }

    /// Display ordering in the editor — Standard first, then MusicBrainz,
    /// then ReplayGain. Matches the grouping a user would expect when
    /// scanning from top to bottom.
    pub fn sort_index(self) -> u8 {
        match self {
            FrameGroup::Standard => 0,
            FrameGroup::MusicBrainz => 1,
            FrameGroup::ReplayGain => 2,
        }
    }
}

/// Enumerate every standard-keyed frame on a file's primary tag.
/// Returns entries sorted by group (Standard → MB → ReplayGain) then by
/// display name within each group, so the editor renders in a stable
/// order that doesn't shuffle when a value changes.
pub fn read_all_frames(path: &Path) -> Result<Vec<FrameEntry>> {
    let tagged_file = lofty::read_from_path(path).map_err(|e| KyokuError::TagRead {
        path: path.to_path_buf(),
        source: e,
    })?;

    let Some(tag) = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())
    else {
        return Ok(Vec::new());
    };

    // Bucket values by ItemKey — lofty stores multi-value fields as
    // multiple TagItems with the same key.
    let mut by_key: std::collections::BTreeMap<String, (ItemKey, Vec<String>)> =
        std::collections::BTreeMap::new();

    for item in tag.items() {
        let key = item.key();
        let value = match item.value() {
            ItemValue::Text(s) | ItemValue::Locator(s) => s.clone(),
            ItemValue::Binary(_) => continue, // covers, pictures, etc. — not editable here
        };
        let display_key = display_name_for(&key);
        by_key
            .entry(display_key)
            .or_insert_with(|| (key, Vec::new()))
            .1
            .push(value);
    }

    let mut frames: Vec<FrameEntry> = by_key
        .into_iter()
        .map(|(display_name, (key, values))| FrameEntry {
            group: group_for(&key),
            key,
            display_name,
            values,
        })
        .collect();

    frames.sort_by(|a, b| {
        a.group
            .sort_index()
            .cmp(&b.group.sort_index())
            .then_with(|| a.display_name.cmp(&b.display_name))
    });

    Ok(frames)
}

/// Classify an `ItemKey` by the feature area it belongs to. Matches the
/// visual grouping in the tag editor. Uses the `Debug` representation as
/// a cheap proxy — every MusicBrainz-* and ReplayGain-* variant's name
/// starts with those prefixes, so this stays correct even as lofty adds
/// new variants.
fn group_for(key: &ItemKey) -> FrameGroup {
    let s = format!("{:?}", key);
    if s.starts_with("MusicBrainz") {
        FrameGroup::MusicBrainz
    } else if s.starts_with("ReplayGain") {
        FrameGroup::ReplayGain
    } else {
        FrameGroup::Standard
    }
}

/// Map an ItemKey to a human-readable label for the editor. Falls back
/// to the Debug form for keys we haven't hand-rolled — those are rare,
/// and the Debug form (`OriginalArtist`, `MusicBrainzReleaseGroupId`,
/// etc.) reads well enough on its own.
pub fn display_name_for(key: &ItemKey) -> String {
    match key {
        ItemKey::TrackTitle => "Title".to_string(),
        ItemKey::TrackArtist => "Artist".to_string(),
        ItemKey::TrackArtists => "Artists".to_string(),
        ItemKey::AlbumTitle => "Album".to_string(),
        ItemKey::AlbumArtist => "Album Artist".to_string(),
        ItemKey::AlbumArtists => "Album Artists".to_string(),
        ItemKey::Genre => "Genre".to_string(),
        ItemKey::Year => "Year".to_string(),
        ItemKey::RecordingDate => "Recording Date".to_string(),
        ItemKey::ReleaseDate => "Release Date".to_string(),
        ItemKey::TrackNumber => "Track #".to_string(),
        ItemKey::TrackTotal => "Tracks Total".to_string(),
        ItemKey::DiscNumber => "Disc #".to_string(),
        ItemKey::DiscTotal => "Discs Total".to_string(),
        ItemKey::Composer => "Composer".to_string(),
        ItemKey::Conductor => "Conductor".to_string(),
        ItemKey::Publisher => "Publisher".to_string(),
        ItemKey::Label => "Label".to_string(),
        ItemKey::Comment => "Comment".to_string(),
        ItemKey::Lyrics => "Lyrics".to_string(),
        ItemKey::Bpm => "BPM".to_string(),
        ItemKey::InitialKey => "Key".to_string(),
        ItemKey::Mood => "Mood".to_string(),
        ItemKey::Isrc => "ISRC".to_string(),
        ItemKey::Barcode => "Barcode".to_string(),
        ItemKey::CatalogNumber => "Catalog #".to_string(),
        ItemKey::EncodedBy => "Encoded By".to_string(),
        ItemKey::CopyrightMessage => "Copyright".to_string(),
        ItemKey::Language => "Language".to_string(),
        // Fallback: debug form, spaced out a little
        other => humanize(&format!("{:?}", other)),
    }
}

/// Turn `MusicBrainzReleaseGroupId` into `MusicBrainz Release Group Id`.
/// Cheap ad-hoc camel-case splitter; only used for the fallback path, so
/// it doesn't need to handle every edge case.
fn humanize(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for (i, c) in camel.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

// ---------- Write ----------

/// Delta to apply to a file's primary tag. `set` replaces any existing
/// values for the given key with the new value set; `unset` removes every
/// item with that key. Applied in order: unsets first, then sets.
#[derive(Debug, Default, Clone)]
pub struct TagChanges {
    pub set: Vec<(ItemKey, TagValue)>,
    pub unset: Vec<ItemKey>,
}

impl TagChanges {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.unset.is_empty()
    }
}

/// A value for a single tag key. `Text` is the common case; `MultiText`
/// preserves multi-value fields (common in Vorbis/APE, supported in
/// ID3v2 via TPE1 null separators).
#[derive(Debug, Clone)]
pub enum TagValue {
    Text(String),
    MultiText(Vec<String>),
}

/// Report returned from a successful `write_tags` call. The counts are
/// informational — the UI surfaces them in the post-save notice so users
/// can tell whether anything actually hit the file.
#[derive(Debug, Clone, Default)]
pub struct TagWriteReport {
    pub fields_written: usize,
    pub fields_removed: usize,
}

/// Apply `changes` to `path`'s primary tag and replace the file
/// atomically.
///
/// Atomicity: lofty's `save_to_path` edits in place, so we first copy
/// the source to `<path>.kyoku.tmp`, apply changes to the copy, then
/// `rename(tmp → path)`. A crash between copy and rename leaves a
/// `.kyoku.tmp` sibling but the original untouched; the rename itself
/// is atomic on POSIX for same-filesystem paths.
///
/// Callers are expected to honour `[tagging] write_tags = false` by not
/// calling this function at all — the flag is a config-level gate and
/// this function does not re-check it.
pub fn write_tags(path: &Path, changes: &TagChanges) -> Result<TagWriteReport> {
    if changes.is_empty() {
        return Ok(TagWriteReport::default());
    }

    let tmp_path = tmp_path_for(path);

    // Copy original to tmp so lofty rewrites the copy, not the live file.
    std::fs::copy(path, &tmp_path)?;

    // Work on a scope so we can clean up the tmp file on error.
    let result = apply_and_save(&tmp_path, changes);

    match result {
        Ok(report) => {
            if let Err(e) = std::fs::rename(&tmp_path, path) {
                // Rename failed — clean up and surface the error.
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e.into());
            }
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

fn apply_and_save(tmp_path: &Path, changes: &TagChanges) -> Result<TagWriteReport> {
    let mut tagged_file =
        lofty::read_from_path(tmp_path).map_err(|e| KyokuError::TagRead {
            path: tmp_path.to_path_buf(),
            source: e,
        })?;

    // Ensure a primary tag exists. `primary_tag_mut()` returns None on a
    // fresh file; in that case we construct the appropriate tag type
    // (ID3v2 for MP3, VorbisComments for FLAC/Ogg, Ilst for M4A, …).
    if tagged_file.primary_tag_mut().is_none() {
        let new_tag = Tag::new(tagged_file.primary_tag_type());
        tagged_file.insert_tag(new_tag);
    }

    let tag = tagged_file
        .primary_tag_mut()
        .expect("primary_tag_mut after insert_tag");

    let mut removed = 0usize;
    let mut written = 0usize;

    for key in &changes.unset {
        // `remove_key` is fire-and-forget — no way to tell if anything
        // matched. Count every requested unset as a removal; the report
        // is informational so a slight over-count is fine.
        tag.remove_key(*key);
        removed += 1;
    }

    for (key, value) in &changes.set {
        // Replace semantics: drop all existing items for this key, then push
        // the new value(s). This is simpler than delta-matching and matches
        // the editor's model (the user sees the full value set and edits it
        // as a unit).
        tag.remove_key(*key);

        let values: Vec<String> = match value {
            TagValue::Text(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![s.clone()]
                }
            }
            TagValue::MultiText(vs) => vs.iter().filter(|v| !v.is_empty()).cloned().collect(),
        };

        if values.is_empty() {
            // Empty Text is treated as "clear this frame" — same as unset.
            removed += 1;
            continue;
        }

        for v in values {
            let pushed = tag.push(TagItem::new(*key, ItemValue::Text(v.clone())));
            if !pushed {
                tracing::warn!(
                    "lofty rejected push for key {:?} (value {:?}) — using push_unchecked",
                    key,
                    v
                );
                tag.push_unchecked(TagItem::new(*key, ItemValue::Text(v)));
            }
        }
        written += 1;
    }

    tagged_file
        .save_to_path(tmp_path, WriteOptions::default())
        .map_err(|e| KyokuError::TagRead {
            path: tmp_path.to_path_buf(),
            source: e,
        })?;

    Ok(TagWriteReport {
        fields_written: written,
        fields_removed: removed,
    })
}

/// Build a sibling path used as the write target before the final
/// rename. The extension is preserved (`song.mp3` → `song.kyoku-tmp.mp3`)
/// so lofty's format detection — which keys off the extension — still
/// works on the tmp file.
fn tmp_path_for(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("tag");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("tmp");
    let mut p = path.to_path_buf();
    p.set_file_name(format!("{}.kyoku-tmp.{}", stem, ext));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format_from_extension() {
        assert_eq!(AudioFormat::from_extension("mp3"), AudioFormat::Mp3);
        assert_eq!(AudioFormat::from_extension("FLAC"), AudioFormat::Flac);
        assert_eq!(AudioFormat::from_extension("m4a"), AudioFormat::M4a);
        assert_eq!(AudioFormat::from_extension("ogg"), AudioFormat::Ogg);
        assert_eq!(AudioFormat::from_extension("opus"), AudioFormat::Opus);
        assert!(!AudioFormat::from_extension("txt").is_supported());
    }

    #[test]
    fn test_read_nonexistent_file() {
        let result = read_track("/nonexistent/file.mp3");
        assert!(result.is_err());
    }

    /// Copy a fixture to a per-test unique temp path so parallel tests
    /// don't stomp on each other's working copies.
    fn fixture_copy(name: &str, test_tag: &str) -> std::path::PathBuf {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample_library")
            .join(name);
        let tmp = std::env::temp_dir().join(format!(
            "kyoku-tagger-{}-{}-{}",
            std::process::id(),
            test_tag,
            name
        ));
        std::fs::copy(&src, &tmp).expect("fixture copy");
        tmp
    }

    #[test]
    fn write_tags_roundtrip_title() {
        let path = fixture_copy("tagged.mp3", "roundtrip_title");
        let mut changes = TagChanges::default();
        changes
            .set
            .push((ItemKey::TrackTitle, TagValue::Text("New Title".into())));
        let report = write_tags(&path, &changes).unwrap();
        assert_eq!(report.fields_written, 1);

        let data = read_tags(&path).unwrap();
        assert_eq!(data.title.as_deref(), Some("New Title"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_tags_unset_removes_frame() {
        let path = fixture_copy("tagged.mp3", "unset_removes");
        let mut changes = TagChanges::default();
        changes.unset.push(ItemKey::Genre);
        // tagged.mp3 fixture has no Genre set, but unset should be a no-op
        // either way — report just reflects whether any item matched.
        let _ = write_tags(&path, &changes).unwrap();

        // Set a genre, then unset it, and verify it's gone.
        let mut changes = TagChanges::default();
        changes
            .set
            .push((ItemKey::Genre, TagValue::Text("Rock".into())));
        write_tags(&path, &changes).unwrap();
        assert_eq!(read_tags(&path).unwrap().genre.as_deref(), Some("Rock"));

        let mut changes = TagChanges::default();
        changes.unset.push(ItemKey::Genre);
        write_tags(&path, &changes).unwrap();
        assert!(read_tags(&path).unwrap().genre.is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_all_frames_returns_sorted_groups() {
        let path = fixture_copy("tagged.mp3", "frames_groups");
        // Seed a Genre (Standard group) and a ReplayGain value
        // (ReplayGain group) to exercise multiple group buckets.
        let mut changes = TagChanges::default();
        changes
            .set
            .push((ItemKey::Genre, TagValue::Text("Experimental".into())));
        changes.set.push((
            ItemKey::ReplayGainAlbumGain,
            TagValue::Text("-7.00 dB".into()),
        ));
        write_tags(&path, &changes).unwrap();

        let frames = read_all_frames(&path).unwrap();

        // Standard frame (Genre) round-trips.
        let g = frames.iter().find(|f| f.key == ItemKey::Genre);
        assert!(g.is_some(), "Genre frame missing after write");
        assert_eq!(g.unwrap().group, FrameGroup::Standard);
        assert_eq!(g.unwrap().values, vec!["Experimental"]);

        // ReplayGain frame round-trips through the TXXX container.
        let rg = frames
            .iter()
            .find(|f| f.key == ItemKey::ReplayGainAlbumGain);
        assert!(rg.is_some(), "ReplayGain frame missing after write");
        assert_eq!(rg.unwrap().group, FrameGroup::ReplayGain);

        // Grouping order is stable: Standard first, then any others.
        let positions: Vec<u8> = frames.iter().map(|f| f.group.sort_index()).collect();
        let mut sorted = positions.clone();
        sorted.sort();
        assert_eq!(positions, sorted);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_tags_noop_when_empty_changes() {
        let path = fixture_copy("tagged.mp3", "noop_empty");
        let before = std::fs::metadata(&path).unwrap().len();
        let report = write_tags(&path, &TagChanges::default()).unwrap();
        assert_eq!(report.fields_written, 0);
        let after = std::fs::metadata(&path).unwrap().len();
        assert_eq!(before, after);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_tags_preserves_other_frames() {
        let path = fixture_copy("tagged.mp3", "preserves_others");
        let before_artist = read_tags(&path).unwrap().artist;

        let mut changes = TagChanges::default();
        changes
            .set
            .push((ItemKey::TrackTitle, TagValue::Text("Something Else".into())));
        write_tags(&path, &changes).unwrap();

        let after = read_tags(&path).unwrap();
        assert_eq!(after.title.as_deref(), Some("Something Else"));
        assert_eq!(after.artist, before_artist);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn humanize_splits_camel_case() {
        assert_eq!(humanize("MusicBrainzReleaseId"), "Music Brainz Release Id");
        assert_eq!(humanize("Year"), "Year");
    }
}
