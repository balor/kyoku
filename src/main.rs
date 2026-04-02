#![warn(clippy::all)]

mod cli;
mod config;
mod core;
mod db;
mod error;
mod external;
mod tui;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::config::Settings;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let config_path = config::paths::config_file();
    let config_exists = config_path.exists();

    // setup and paths work without a config file; everything else requires one
    let needs_config = !matches!(
        cli.command,
        Some(Command::Setup) | Some(Command::Paths) | Some(Command::Info { .. }) | None
    );

    if needs_config && !config_exists {
        eprintln!("No config file found at {}", config_path.display());
        eprintln!();
        eprintln!("Run `kyoku setup` to create one.");
        std::process::exit(1);
    }

    let settings = Settings::load(&config_path)?;

    match cli.command {
        None => {
            if !config_exists {
                println!("Welcome to kyoku!");
                println!();
                println!("Run `kyoku setup` to get started.");
            } else {
                // Launch TUI (future milestone)
                println!("kyoku TUI — not yet implemented. Use a subcommand.");
                println!("Run `kyoku --help` for available commands.");
            }
        }
        Some(Command::Import {
            path,
            pretend,
            no_match,
            loose,
            collection,
            ..
        }) => {
            let _db = db::open_database(config::paths::database_file())?;
            println!(
                "Import from {} (pretend={}, no_match={}, loose={}, collection={:?})",
                path.display(),
                pretend,
                no_match,
                loose,
                collection,
            );
            println!("Import pipeline — not yet implemented (milestone 2).");
        }
        Some(Command::Info { path }) => {
            let track = core::tagger::read_track(&path)?;
            println!("File:        {}", track.file_path.display());
            println!("Format:      {}", track.file_format.as_str());
            println!("Title:       {}", track.title);
            println!(
                "Artist:      {}",
                track.artist.as_deref().unwrap_or("(none)")
            );
            if let Some(ref tag_data) = core::tagger::read_tags(&path).ok() {
                println!(
                    "Album:       {}",
                    tag_data.album.as_deref().unwrap_or("(none)")
                );
                println!(
                    "Album Artist: {}",
                    tag_data.album_artist.as_deref().unwrap_or("(none)")
                );
                println!(
                    "Year:        {}",
                    tag_data
                        .year
                        .map(|y| y.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                );
                println!(
                    "Genre:       {}",
                    tag_data.genre.as_deref().unwrap_or("(none)")
                );
            }
            println!(
                "Track:       {}",
                track
                    .track_number
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "(none)".to_string())
            );
            println!("Disc:        {}", track.disc_number);
            if let Some(ms) = track.duration_ms {
                let secs = ms / 1000;
                println!("Duration:    {}:{:02}", secs / 60, secs % 60);
            }
            if let Some(br) = track.bitrate {
                println!("Bitrate:     {} kbps", br);
            }
            if let Some(sr) = track.sample_rate {
                println!("Sample Rate: {} Hz", sr);
            }
            println!("Status:      {}", track.tag_status.as_str());
        }
        Some(Command::Setup) => {
            cli::setup::run(settings)?;
        }
        Some(Command::Paths) => {
            let config = config::paths::config_file();
            let db = config::paths::database_file();
            let cache = config::paths::cache_dir();
            let music = &settings.library.music_dir;

            let inboxes = &settings.library.inbox_dirs;

            println!("config:   {}", config.display());
            println!("database: {}", db.display());
            println!("cache:    {}", cache.display());
            println!("music:    {}", music.display());
            if inboxes.is_empty() {
                println!("inboxes:  (none)");
            } else {
                let paths: Vec<_> = inboxes.iter().map(|p| p.display().to_string()).collect();
                println!("inboxes:  {}", paths.join(", "));
            }

            if !config.exists() {
                println!("\n(config file does not exist yet — using defaults)");
            }
        }
        Some(Command::Scan) => {
            let _db = db::open_database(config::paths::database_file())?;
            println!(
                "Scanning inbox directories: {:?}",
                settings.library.inbox_dirs
            );
            println!("Scan — not yet implemented (milestone 2).");
        }
        Some(Command::Organize { .. }) => {
            println!("Organize — not yet implemented (milestone 5).");
        }
        Some(Command::Relocate { .. }) => {
            println!("Relocate — not yet implemented (milestone 5).");
        }
    }

    Ok(())
}
