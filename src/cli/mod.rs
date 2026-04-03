pub mod setup;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "kyoku", version, about = "TUI-first music library manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Import audio files into the library database (defaults to inbox dirs if no path given)
    Import {
        /// Path to scan for audio files (defaults to inbox dirs if omitted)
        path: Option<PathBuf>,

        /// Dry run: show what would happen without modifying anything
        #[arg(long, short = 'p')]
        pretend: bool,

        /// Auto-accept matches above threshold, skip below
        #[arg(long, short = 'a')]
        auto: bool,

        /// Skip AcoustID fingerprinting
        #[arg(long)]
        no_fingerprint: bool,

        /// Skip MusicBrainz matching entirely (import as-is)
        #[arg(long)]
        no_match: bool,

        /// Treat all files as individual tracks, don't group into albums
        #[arg(long)]
        loose: bool,

        /// Add all imported tracks to a collection (creates it if needed)
        #[arg(long)]
        collection: Option<String>,
    },

    /// Display audio file metadata and tags
    Info {
        /// Path to the audio file
        path: PathBuf,
    },

    /// Interactive setup wizard — configure music dir, inbox dirs, and more
    Setup,

    /// Show resolved config, data, and cache paths
    Paths,

    /// Scan inbox directories for unimported files
    Scan,

    /// Preview and apply file reorganization
    Organize {
        /// Actually move files (requires confirmation)
        #[arg(long)]
        apply: bool,

        /// Organize specific artist only
        #[arg(long)]
        artist: Option<String>,

        /// Organize specific album only
        #[arg(long)]
        album: Option<String>,

        /// Organize only files under this path
        #[arg(long)]
        path: Option<PathBuf>,

        /// Organize all tracks in a collection
        #[arg(long)]
        collection: Option<String>,
    },

    /// Rebase all library paths when music moves to a new location
    Relocate {
        /// Old path prefix
        old_prefix: Option<PathBuf>,

        /// New path prefix
        new_prefix: Option<PathBuf>,

        /// Check all DB paths exist on disk, report missing
        #[arg(long)]
        verify: bool,

        /// Preview changes without applying
        #[arg(long)]
        pretend: bool,
    },
}
