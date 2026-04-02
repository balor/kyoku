use std::path::PathBuf;

/// Resolve the config directory for kyoku.
/// Uses $XDG_CONFIG_HOME/kyoku or platform default.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("kyoku")
}

/// Resolve the data directory for kyoku (database lives here).
/// Uses $XDG_DATA_HOME/kyoku or platform default.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("kyoku")
}

/// Resolve the cache directory for kyoku.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("~/.cache"))
        .join("kyoku")
}

/// Path to the config file.
pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// Path to the library database.
pub fn database_file() -> PathBuf {
    data_dir().join("library.db")
}
