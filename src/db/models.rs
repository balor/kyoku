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

/// Audio extensions that kyoku recognizes.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "m4a", "aac", "mp4", "wav", "wma", "ape", "opus", "aiff", "aif",
];
