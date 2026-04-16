use std::path::PathBuf;

/// Unified error type for kyoku.
#[derive(Debug, thiserror::Error)]
pub enum KyokuError {
    #[error("File not found: {path}")]
    FileNotFound { path: PathBuf },

    #[error("Unsupported audio format: {ext}")]
    UnsupportedFormat { ext: String },

    #[error("Tag read error for {path}: {source}")]
    TagRead {
        path: PathBuf,
        #[source]
        source: lofty::error::LoftyError,
    },

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, KyokuError>;
