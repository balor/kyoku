pub mod setup;

use clap::{ArgGroup, Parser, Subcommand};
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
    #[command(group(
        ArgGroup::new("organize-filter")
            .args(["artist", "album", "path", "collection"])
            .multiple(false)
    ))]
    Organize {
        /// Actually move files (requires confirmation)
        #[arg(long)]
        apply: bool,

        /// Skip the apply confirmation prompt (for scripts)
        #[arg(long, short = 'y', requires = "apply")]
        yes: bool,

        /// Explicit dry run alias (default behavior without --apply)
        #[arg(long, conflicts_with = "apply")]
        pretend: bool,

        /// Show per-file from/to paths instead of grouped summary
        #[arg(long, short = 'd')]
        details: bool,

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn organize_filters_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from(["kyoku", "organize", "--artist", "A", "--album", "B",]).is_err()
        );
    }

    #[test]
    fn organize_pretend_conflicts_with_apply() {
        assert!(Cli::try_parse_from(["kyoku", "organize", "--apply", "--pretend"]).is_err());
    }

    #[test]
    fn organize_yes_requires_apply() {
        assert!(Cli::try_parse_from(["kyoku", "organize", "--yes"]).is_err());
        assert!(Cli::try_parse_from(["kyoku", "organize", "--apply", "--yes"]).is_ok());
    }
}
