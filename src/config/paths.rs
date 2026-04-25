use std::path::{Path, PathBuf};

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

/// Expand `~` at the start of a path to the user's home directory.
pub fn expand_tilde(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    path.to_path_buf()
}

/// Verify that `path` is usable as the managed music library: it must
/// exist, be a directory, and accept writes. Returns a short reason on
/// failure. We actually try to create-and-remove a probe file because
/// `Permissions::readonly()` misses macOS ACLs and POSIX group/other
/// bits that apply only to our effective uid.
pub fn validate_library_dir(path: &Path) -> std::result::Result<(), String> {
    if !path.exists() {
        return Err(format!("does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()));
    }
    let probe = path.join(".kyoku-write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!("not writable ({})", e)),
    }
}
