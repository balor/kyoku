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
    pub player: PlayerSettings,

    #[serde(default)]
    pub ui: UiSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarySettings {
    #[serde(default = "default_music_dir")]
    pub music_dir: PathBuf,

    /// Directory holding `library.db`. Defaults to the platform data dir
    /// (XDG / macOS Application Support / Windows AppData) but is freely
    /// reconfigurable — common reason to override is keeping the DB next
    /// to the music it indexes (e.g. on the same external drive).
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    #[serde(default)]
    pub inbox_dirs: Vec<PathBuf>,

    #[serde(default = "default_path_template")]
    pub path_template: String,

    #[serde(default = "default_path_template_single_disc")]
    pub path_template_single_disc: String,

    /// Default template for collection copies (when a collection has no override).
    #[serde(default = "default_collection_path_template")]
    pub collection_path_template: String,

    /// Template for loose tracks (no album, no collection).
    #[serde(default = "default_loose_path_template")]
    pub loose_path_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSettings {
    #[serde(default)]
    pub organize_operation: OrganizeOperation,

    /// Auto-accept MusicBrainz matches at or above this similarity (0.0 - 1.0).
    /// Applied to the top candidate during the Review step.
    #[serde(default = "default_auto_match_threshold")]
    pub auto_match_threshold: f64,

    /// Number of MusicBrainz match candidates fetched per group on search.
    #[serde(default = "default_match_candidates")]
    pub match_candidates: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggingSettings {
    #[serde(default = "default_true")]
    pub write_tags: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicBrainzSettings {
    #[serde(default = "default_rate_limit")]
    pub rate_limit_ms: u64,

    /// Preferred script for artist & album names written from MusicBrainz matches.
    ///
    /// `Native` uses MB's canonical credit/title as-is (current default).
    /// `Latin` prefers a Latin-script alias when one exists (e.g. `Yorushika`
    /// over `ヨルシカ`). Falls back to canonical when no alias matches.
    #[serde(default)]
    pub name_script: NameScriptPreference,

    /// Cover Art Archive download size for the `C` fetch in album detail.
    /// CAA exposes a few fixed-width thumbnails plus the original upload.
    /// `Px500` is small but loads fast and stays well under 100 KB on disk;
    /// `Px1200` is a sensible quality/size trade for media-server use;
    /// `Original` gets whatever the uploader provided (often 1500-3000 px,
    /// sometimes multiple MB) and falls back to `Px1200` then `Px500` when
    /// no original is archived.
    #[serde(default)]
    pub cover_art_size: CoverArtSize,
}

/// Width preset for Cover Art Archive front-cover downloads.
///
/// CAA serves these at fixed URL suffixes (`/release/{mbid}/front-N`); the
/// `Original` variant maps to `/release/{mbid}/front` (no suffix), which
/// returns whatever the uploader provided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverArtSize {
    #[serde(rename = "250")]
    Px250,
    #[default]
    #[serde(rename = "500")]
    Px500,
    #[serde(rename = "1200")]
    Px1200,
    #[serde(rename = "original")]
    Original,
}

impl CoverArtSize {
    /// URL fragment used after `/release/{mbid}/front` (or empty for
    /// `Original`, which uses `/front` directly).
    pub fn url_suffix(self) -> &'static str {
        match self {
            CoverArtSize::Px250 => "-250",
            CoverArtSize::Px500 => "-500",
            CoverArtSize::Px1200 => "-1200",
            CoverArtSize::Original => "",
        }
    }
}

/// User preference for which script variant of MB-derived artist / album
/// names should land in the DB, tags, and filesystem tree. Applied only to
/// artist names (track.artist + album.album_artist) and album titles — track
/// titles stay as MB returns them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NameScriptPreference {
    /// Use MB's canonical name (whatever `artist-credit[0].name` / release
    /// `title` returns — often native script for JP/KR/etc. releases).
    #[default]
    Native,
    /// Prefer a Latin-script alias when available; fall back to canonical.
    Latin,
}

/// External music player used by the `p`/`P` keys and `kyoku play`.
///
/// Resolution order: `command` (full argv template) wins, then `app`
/// (macOS only), then built-in auto-detection (mpv, VLC, IINA, fb2k, …),
/// then the OS default handler (xdg-open / open / explorer.exe).
/// Both fields unset = auto-detect.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayerSettings {
    /// Full argv template, e.g. `["mpv", "--playlist={playlist}"]`.
    /// Placeholders: `{playlist}` (path to the generated .m3u8 or the
    /// single audio file), `{files}` (expands to one argv item per file),
    /// `{files-csv}` (comma-joined, for Quod Libet-style players). With
    /// no placeholder, the target path(s) are appended as trailing args.
    /// On Windows, argv[0] must be a real executable path — `.bat`/`.cmd`
    /// wrappers can't be spawned directly (use `["cmd", "/C", ...]`).
    #[serde(default)]
    pub command: Option<Vec<String>>,

    /// macOS only: app name launched via `open -a <app> <target>`.
    /// Ignored on other platforms.
    #[serde(default)]
    pub app: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Draw the album cover preview in album detail view. Set to `false`
    /// when running inside a terminal/multiplexer combination that can't
    /// render halfblock graphics cleanly (some zellij + native-protocol
    /// terminals show a blank gap where the cover should be).
    #[serde(default = "default_true")]
    pub show_cover_preview: bool,
}

// Default value functions

fn default_music_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join("Music")
}

fn default_data_dir() -> PathBuf {
    crate::config::paths::default_data_dir()
}

fn default_path_template() -> String {
    "{album_artist}/{album} ({year})/{disc:0}-{track:02} {title}.{ext}".to_string()
}

fn default_path_template_single_disc() -> String {
    "{album_artist}/{album} ({year})/{track:02} {title}.{ext}".to_string()
}

fn default_collection_path_template() -> String {
    "Collections/{collection}/{position:02} {album_artist} - {title}.{ext}".to_string()
}

fn default_loose_path_template() -> String {
    "_loose/{artist} - {title}.{ext}".to_string()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrganizeOperation {
    #[default]
    Move,
    Copy,
}

fn default_auto_match_threshold() -> f64 {
    0.85
}

fn default_true() -> bool {
    true
}

fn default_match_candidates() -> u32 {
    5
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
            player: PlayerSettings::default(),
            ui: UiSettings::default(),
        }
    }
}

impl Settings {
    fn default_library() -> LibrarySettings {
        LibrarySettings {
            music_dir: default_music_dir(),
            data_dir: default_data_dir(),
            inbox_dirs: Vec::new(),
            path_template: default_path_template(),
            path_template_single_disc: default_path_template_single_disc(),
            collection_path_template: default_collection_path_template(),
            loose_path_template: default_loose_path_template(),
        }
    }

    /// Resolved path to the SQLite database file — `library.data_dir`
    /// joined with `library.db`. Use this everywhere instead of the
    /// platform-default helper so the user's override wins.
    pub fn database_file(&self) -> PathBuf {
        self.library
            .data_dir
            .join(crate::config::paths::DATABASE_FILENAME)
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
        let mut settings: Self =
            toml::from_str(&content).map_err(|e| KyokuError::Config(e.to_string()))?;

        // Expand tilde in paths
        settings.library.music_dir =
            crate::config::paths::expand_tilde(&settings.library.music_dir);
        settings.library.data_dir = crate::config::paths::expand_tilde(&settings.library.data_dir);
        settings.library.inbox_dirs = settings
            .library
            .inbox_dirs
            .into_iter()
            .map(|p| crate::config::paths::expand_tilde(&p))
            .collect();

        if !(0.0..=1.0).contains(&settings.import.auto_match_threshold) {
            let original = settings.import.auto_match_threshold;
            settings.import.auto_match_threshold = original.clamp(0.0, 1.0);
            tracing::warn!(
                "import.auto_match_threshold {} is outside 0.0..=1.0; clamped to {}",
                original,
                settings.import.auto_match_threshold
            );
        }
        if settings.musicbrainz.rate_limit_ms < 1000 {
            let original = settings.musicbrainz.rate_limit_ms;
            settings.musicbrainz.rate_limit_ms = 1000;
            tracing::warn!(
                "musicbrainz.rate_limit_ms {} is below MusicBrainz policy floor; using 1000",
                original
            );
        }

        Ok(settings)
    }
}

impl Default for ImportSettings {
    fn default() -> Self {
        Self {
            organize_operation: OrganizeOperation::default(),
            auto_match_threshold: default_auto_match_threshold(),
            match_candidates: default_match_candidates(),
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
            rate_limit_ms: default_rate_limit(),
            name_script: NameScriptPreference::default(),
            cover_art_size: CoverArtSize::default(),
        }
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            show_cover_preview: true,
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
        assert_eq!(settings.import.auto_match_threshold, 0.85);
        assert_eq!(settings.import.match_candidates, 5);
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
match_candidates = 3

[tagging]
write_tags = false

[ui]
theme = "kanagawa"
"#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(settings.library.music_dir, PathBuf::from("/data/music"));
        assert_eq!(settings.library.inbox_dirs.len(), 1);
        assert_eq!(settings.import.organize_operation, OrganizeOperation::Copy);
        assert_eq!(settings.import.match_candidates, 3);
        assert!(!settings.tagging.write_tags);
        assert_eq!(settings.ui.theme, "kanagawa");
    }

    #[test]
    fn test_load_nonexistent_file() {
        let settings = Settings::load("/nonexistent/config.toml").unwrap();
        assert_eq!(settings.ui.theme, "tokyo-night");
    }

    #[test]
    fn load_clamps_out_of_range_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[import]
auto_match_threshold = 1.5

[musicbrainz]
rate_limit_ms = 0
"#,
        )
        .unwrap();

        let settings = Settings::load(&path).unwrap();

        assert_eq!(settings.import.auto_match_threshold, 1.0);
        assert_eq!(settings.musicbrainz.rate_limit_ms, 1000);
    }

    #[test]
    fn invalid_organize_operation_rejects_config() {
        let toml = r#"
[import]
organize_operation = "cp"
"#;

        assert!(toml::from_str::<Settings>(toml).is_err());
    }
}
