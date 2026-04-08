use std::path::Path;
use std::time::Duration;

use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::tag::Accessor;

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
            t.get_string(lofty::tag::ItemKey::AlbumArtist)
                .map(|s| s.to_string())
        }),
        year: tag.and_then(|t| {
            // year() was removed in lofty 0.23, use get_string with Year key
            t.get_string(lofty::tag::ItemKey::Year)
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

/// Write tag data to an audio file.
///
/// Only writes fields that are `Some` — leaves other fields untouched.
/// Uses the primary tag format for the file (ID3v2 for MP3, VorbisComments for FLAC/OGG, etc.)
pub fn write_tags(path: impl AsRef<Path>, data: &TagData) -> Result<()> {
    use lofty::config::WriteOptions;
    use lofty::file::AudioFile;

    let path = path.as_ref();

    let mut tagged_file = lofty::read_from_path(path).map_err(|e| KyokuError::TagWrite {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Get the primary tag type for this format, or fall back to the first tag
    let tag_type = tagged_file
        .primary_tag()
        .map(|t| t.tag_type())
        .or_else(|| tagged_file.first_tag().map(|t| t.tag_type()));

    let tag_type = match tag_type {
        Some(tt) => tt,
        None => {
            // No existing tag — use the primary tag type for this format
            tagged_file.file_type().primary_tag_type()
        }
    };

    // Get or insert the tag
    if tagged_file.tag(tag_type).is_none() {
        tagged_file.insert_tag(lofty::tag::Tag::new(tag_type));
    }
    let tag = tagged_file.tag_mut(tag_type).unwrap();

    if let Some(title) = &data.title {
        tag.set_title(title.clone());
    }
    if let Some(artist) = &data.artist {
        tag.set_artist(artist.clone());
    }
    if let Some(album) = &data.album {
        tag.set_album(album.clone());
    }
    if let Some(album_artist) = &data.album_artist {
        tag.push(lofty::tag::TagItem::new(
            lofty::tag::ItemKey::AlbumArtist,
            lofty::tag::ItemValue::Text(album_artist.clone()),
        ));
    }
    if let Some(year) = data.year {
        tag.push(lofty::tag::TagItem::new(
            lofty::tag::ItemKey::Year,
            lofty::tag::ItemValue::Text(year.to_string()),
        ));
    }
    if let Some(track) = data.track_number {
        tag.set_track(track);
    }
    if let Some(disc) = data.disc_number {
        tag.set_disk(disc);
    }
    if let Some(genre) = &data.genre {
        tag.set_genre(genre.clone());
    }

    // Write back using save_to_path (works on TaggedFile)
    tagged_file
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| KyokuError::TagWrite {
            path: path.to_path_buf(),
            source: e,
        })?;

    Ok(())
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
        assert!(AudioFormat::from_extension("txt").is_supported() == false);
    }

    #[test]
    fn test_read_nonexistent_file() {
        let result = read_track("/nonexistent/file.mp3");
        assert!(result.is_err());
    }
}
