use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Option<i64>,
    pub album_id: Option<i64>,
    pub title: String,
    pub artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: u32,
    pub duration_ms: Option<u64>,
    pub mbid: Option<String>,
    pub file_path: PathBuf,
    pub file_format: AudioFormat,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub tag_status: TagStatus,
    pub source_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TagStatus {
    Unmatched,
    Matched,
    Verified,
    Manual,
}

impl TagStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unmatched => "unmatched",
            Self::Matched => "matched",
            Self::Verified => "verified",
            Self::Manual => "manual",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "matched" => Self::Matched,
            "verified" => Self::Verified,
            "manual" => Self::Manual,
            _ => Self::Unmatched,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AlbumType {
    Album,
    Compilation,
    Single,
    Ep,
    Soundtrack,
    Live,
    Other,
}

impl AlbumType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Compilation => "compilation",
            Self::Single => "single",
            Self::Ep => "ep",
            Self::Soundtrack => "soundtrack",
            Self::Live => "live",
            Self::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "compilation" => Self::Compilation,
            "single" => Self::Single,
            "ep" => Self::Ep,
            "soundtrack" => Self::Soundtrack,
            "live" => Self::Live,
            "other" => Self::Other,
            _ => Self::Album,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    Flac,
    Ogg,
    M4a,
    Wav,
    Wma,
    Ape,
    Opus,
    Aiff,
    Unknown(String),
}

impl AudioFormat {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "mp3" => Self::Mp3,
            "flac" => Self::Flac,
            "ogg" | "oga" => Self::Ogg,
            "m4a" | "aac" | "mp4" => Self::M4a,
            "wav" => Self::Wav,
            "wma" => Self::Wma,
            "ape" => Self::Ape,
            "opus" => Self::Opus,
            "aiff" | "aif" => Self::Aiff,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Wma => "wma",
            Self::Ape => "ape",
            Self::Opus => "opus",
            Self::Aiff => "aiff",
            Self::Unknown(s) => s,
        }
    }

    /// Returns true if this is a known supported audio format.
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: Option<i64>,
    pub title: String,
    pub album_artist: Option<String>,
    pub year: Option<i32>,
    pub mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub disc_total: u32,
    pub track_total: Option<u32>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub media_type: Option<String>,
    pub album_type: AlbumType,
    pub cover_art_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    pub path_template: Option<String>,
    pub track_count: u32,
}

/// Audio extensions that kyoku recognizes.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "m4a", "aac", "mp4", "wav", "wma", "ape", "opus", "aiff", "aif",
];
