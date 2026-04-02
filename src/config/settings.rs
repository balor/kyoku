use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::{KyokuError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "Settings::default_library")]
    pub library: LibrarySettings,

    #[serde(default)]
    pub import: ImportSettings,

    #[serde(default)]
    pub tagging: TaggingSettings,

    #[serde(default)]
    pub musicbrainz: MusicBrainzSettings,

    #[serde(default)]
    pub acoustid: AcoustIdSettings,

    #[serde(default)]
    pub ui: UiSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarySettings {
    #[serde(default = "default_music_dir")]
    pub music_dir: PathBuf,

    #[serde(default)]
    pub inbox_dirs: Vec<PathBuf>,

    #[serde(default = "default_path_template")]
    pub path_template: String,

    #[serde(default = "default_path_template_single_disc")]
    pub path_template_single_disc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSettings {
    #[serde(default = "default_organize_operation")]
    pub organize_operation: String,

    #[serde(default = "default_auto_match_threshold")]
    pub auto_match_threshold: f64,

    #[serde(default = "default_true")]
    pub use_fingerprint: bool,

    #[serde(default = "default_match_candidates")]
    pub match_candidates: u32,

    #[serde(default = "default_true")]
    pub skip_duplicates: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggingSettings {
    #[serde(default = "default_true")]
    pub write_tags: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicBrainzSettings {
    #[serde(default = "default_user_agent")]
    pub user_agent: String,

    #[serde(default = "default_rate_limit")]
    pub rate_limit_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcoustIdSettings {
    #[serde(default)]
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    #[serde(default = "default_theme")]
    pub theme: String,
}

// Default value functions

fn default_music_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join("Music")
}

fn default_path_template() -> String {
    "{album_artist}/{album} ({year})/{disc:0}-{track:02} {title}.{ext}".to_string()
}

fn default_path_template_single_disc() -> String {
    "{album_artist}/{album} ({year})/{track:02} {title}.{ext}".to_string()
}

fn default_organize_operation() -> String {
    "move".to_string()
}

fn default_auto_match_threshold() -> f64 {
    0.95
}

fn default_true() -> bool {
    true
}

fn default_match_candidates() -> u32 {
    5
}

fn default_user_agent() -> String {
    "kyoku/0.1.0 (https://github.com/yourname/kyoku)".to_string()
}

fn default_rate_limit() -> u64 {
    1100
}

fn default_theme() -> String {
    "tokyo-night".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            library: Settings::default_library(),
            import: ImportSettings::default(),
            tagging: TaggingSettings::default(),
            musicbrainz: MusicBrainzSettings::default(),
            acoustid: AcoustIdSettings::default(),
            ui: UiSettings::default(),
        }
    }
}

impl Settings {
    fn default_library() -> LibrarySettings {
        LibrarySettings {
            music_dir: default_music_dir(),
            inbox_dirs: Vec::new(),
            path_template: default_path_template(),
            path_template_single_disc: default_path_template_single_disc(),
        }
    }

    /// Load settings from a TOML file. Falls back to defaults if the file doesn't exist.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            tracing::debug!(
                "Config file not found at {}, using defaults",
                path.display()
            );
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let settings: Self =
            toml::from_str(&content).map_err(|e| KyokuError::Config(e.to_string()))?;
        Ok(settings)
    }
}

impl Default for ImportSettings {
    fn default() -> Self {
        Self {
            organize_operation: default_organize_operation(),
            auto_match_threshold: default_auto_match_threshold(),
            use_fingerprint: true,
            match_candidates: default_match_candidates(),
            skip_duplicates: true,
        }
    }
}

impl Default for TaggingSettings {
    fn default() -> Self {
        Self { write_tags: true }
    }
}

impl Default for MusicBrainzSettings {
    fn default() -> Self {
        Self {
            user_agent: default_user_agent(),
            rate_limit_ms: default_rate_limit(),
        }
    }
}

impl Default for AcoustIdSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
        }
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: default_theme(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = Settings::default();
        assert_eq!(settings.ui.theme, "tokyo-night");
        assert!(settings.import.skip_duplicates);
        assert_eq!(settings.import.auto_match_threshold, 0.95);
    }

    #[test]
    fn test_parse_minimal_toml() {
        let toml = r#"
[library]
music_dir = "/home/user/Music"
"#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(
            settings.library.music_dir,
            PathBuf::from("/home/user/Music")
        );
        assert_eq!(settings.ui.theme, "tokyo-night");
    }

    #[test]
    fn test_parse_full_toml() {
        let toml = r#"
[library]
music_dir = "/data/music"
inbox_dirs = ["/home/user/Downloads"]
path_template = "{artist}/{album}/{track:02} {title}.{ext}"
path_template_single_disc = "{artist}/{album}/{track:02} {title}.{ext}"

[import]
organize_operation = "copy"
auto_match_threshold = 0.90
use_fingerprint = false
match_candidates = 3
skip_duplicates = false

[tagging]
write_tags = false

[ui]
theme = "kanagawa"
"#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(settings.library.music_dir, PathBuf::from("/data/music"));
        assert_eq!(settings.library.inbox_dirs.len(), 1);
        assert_eq!(settings.import.organize_operation, "copy");
        assert!(!settings.import.use_fingerprint);
        assert!(!settings.tagging.write_tags);
        assert_eq!(settings.ui.theme, "kanagawa");
    }

    #[test]
    fn test_load_nonexistent_file() {
        let settings = Settings::load("/nonexistent/config.toml").unwrap();
        assert_eq!(settings.ui.theme, "tokyo-night");
    }
}
