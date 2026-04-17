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
    // Default to info-level for our own crate so CLI commands print their
    // progress messages out of the box; RUST_LOG overrides when set.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .with_target(false)
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
                let conn = db::open_database(config::paths::database_file())?;
                tui::run(conn, settings)?;
            }
        }
        Some(Command::Import {
            path,
            pretend,
            loose,
            collection,
            ..
        }) => {
            let conn = db::open_database(config::paths::database_file())?;

            let paths: Vec<std::path::PathBuf> = match path {
                Some(p) => vec![p],
                None => {
                    let inbox = &settings.library.inbox_dirs;
                    if inbox.is_empty() {
                        eprintln!("No path given and no inbox directories configured.");
                        eprintln!("Either provide a path: kyoku import <path>");
                        eprintln!("Or add inbox_dirs to your config (see `kyoku paths`).");
                        std::process::exit(1);
                    }
                    println!("Importing from inbox directories...");
                    inbox.clone()
                }
            };

            let mut total = core::importer::ImportResult::default();
            for import_path in &paths {
                if paths.len() > 1 {
                    println!("\n--- {} ---", import_path.display());
                }
                let result = core::importer::import(
                    &conn,
                    import_path,
                    loose,
                    pretend,
                    collection.as_deref(),
                )?;
                total.imported += result.imported;
                total.skipped_duplicate += result.skipped_duplicate;
                total.skipped_error += result.skipped_error;
                total.added_to_collection += result.added_to_collection;
                total.albums_created += result.albums_created;
                total.albums_existing += result.albums_existing;
                total.collection_created |= result.collection_created;
                total.errors.extend(result.errors);
            }

            println!();
            if total.albums_created > 0 {
                println!("Albums created: {}", total.albums_created);
            }
            if total.albums_existing > 0 {
                println!("Added to existing albums: {}", total.albums_existing);
            }
            if let Some(ref name) = collection {
                let newly_imported = total.imported;
                let existing = total.added_to_collection;
                if newly_imported > 0 || existing > 0 {
                    let label = if total.collection_created {
                        "Collection created"
                    } else {
                        "Collection"
                    };
                    let mut parts = Vec::new();
                    if newly_imported > 0 {
                        parts.push(format!(
                            "{} {}",
                            newly_imported,
                            if newly_imported == 1 { "track" } else { "tracks" }
                        ));
                    }
                    if existing > 0 {
                        parts.push(format!(
                            "{} existing {} added",
                            existing,
                            if existing == 1 { "track" } else { "tracks" }
                        ));
                    }
                    println!("{}: {} ({})", label, name, parts.join(", "));
                }
            }
            println!(
                "Import complete: {} imported, {} skipped (duplicate), {} errors",
                total.imported, total.skipped_duplicate, total.skipped_error
            );
            if !total.errors.is_empty() {
                println!("\nErrors:");
                for (path, err) in &total.errors {
                    println!("  {} — {}", path, err);
                }
            }
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
            let conn = db::open_database(config::paths::database_file())?;
            let inbox_dirs = &settings.library.inbox_dirs;

            if inbox_dirs.is_empty() {
                println!("No inbox directories configured.");
                println!("Add inbox_dirs to your config (see `kyoku paths`).");
            } else {
                let unimported = core::importer::scan_inbox(&conn, inbox_dirs)?;
                if unimported.is_empty() {
                    println!("No new files found in inbox directories.");
                } else {
                    println!("Found {} unimported file(s):", unimported.len());
                    for path in &unimported {
                        println!("  {}", path.display());
                    }
                    println!();
                    println!("Run `kyoku import <path>` to import them.");
                }
            }
        }
        Some(Command::Organize {
            apply,
            artist,
            album,
            path,
            collection,
        }) => {
            let conn = db::open_database(config::paths::database_file())?;
            let filter = if let Some(a) = artist {
                core::organizer::OrganizeFilter::Artist(a)
            } else if let Some(a) = album {
                core::organizer::OrganizeFilter::Album(a)
            } else if let Some(p) = path {
                core::organizer::OrganizeFilter::Path(p)
            } else if let Some(c) = collection {
                core::organizer::OrganizeFilter::Collection(c)
            } else {
                core::organizer::OrganizeFilter::All
            };

            let plan = core::organizer::plan_organize(&conn, &settings, filter)?;

            if plan.moves.is_empty()
                && plan.copies.is_empty()
                && plan.missing_sources.is_empty()
            {
                println!(
                    "Nothing to do — {} file(s) already in the correct location.",
                    plan.skipped
                );
            } else {
                // Group moves by source dir → target dir for a compact display
                let mut dir_groups: std::collections::BTreeMap<
                    (String, String),
                    Vec<String>,
                > = std::collections::BTreeMap::new();
                for m in &plan.moves {
                    let from_dir = m
                        .from
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let to_dir = m
                        .to
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let filename = m
                        .to
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("?")
                        .to_string();
                    dir_groups
                        .entry((from_dir, to_dir))
                        .or_default()
                        .push(filename);
                }

                println!("Organize plan:\n");
                for ((from_dir, to_dir), files) in &dir_groups {
                    println!("  {} ({} files)", from_dir, files.len());
                    println!("  → {}\n", to_dir);
                }

                // Group copies by collection + target dir
                let mut copy_groups: std::collections::BTreeMap<(String, String), usize> =
                    std::collections::BTreeMap::new();
                for c in &plan.copies {
                    let to_dir = c
                        .to
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    *copy_groups
                        .entry((c.collection_name.clone(), to_dir))
                        .or_insert(0) += 1;
                }
                for ((coll, to_dir), count) in &copy_groups {
                    println!("  copy ({} files) → {} (collection: {})", count, to_dir, coll);
                }
                if !copy_groups.is_empty() {
                    println!();
                }

                if !plan.missing_sources.is_empty() {
                    println!(
                        "\nMissing source files ({} — DB rows will be pruned):",
                        plan.missing_sources.len()
                    );
                    for (id, path, title) in plan.missing_sources.iter().take(10) {
                        println!("  [{}] {} — {}", id, title, path.display());
                    }
                    if plan.missing_sources.len() > 10 {
                        println!("  … and {} more", plan.missing_sources.len() - 10);
                    }
                    println!();
                }

                println!(
                    "{} file(s) to move, {} to copy, {} already in place, {} orphaned",
                    plan.moves.len(),
                    plan.copies.len(),
                    plan.skipped,
                    plan.missing_sources.len(),
                );

                if apply {
                    // Check music_dir exists
                    if !settings.library.music_dir.exists() {
                        print!(
                            "Music directory {} does not exist. Create it? [y/N] ",
                            settings.library.music_dir.display()
                        );
                        use std::io::Write;
                        std::io::stdout().flush()?;
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("Aborted.");
                            std::process::exit(0);
                        }
                        std::fs::create_dir_all(&settings.library.music_dir)?;
                    }

                    let result = core::organizer::apply_organize(
                        &conn,
                        &plan,
                        &settings.import.organize_operation,
                    )?;
                    println!();
                    println!(
                        "Done: {} moved, {} copied, {} dirs cleaned, {} orphans pruned",
                        result.moved,
                        result.copied,
                        result.dirs_cleaned,
                        result.orphans_cleaned,
                    );
                    if !result.errors.is_empty() {
                        println!("Errors:");
                        for (path, err) in &result.errors {
                            println!("  {} — {}", path, err);
                        }
                    }
                } else {
                    println!("(dry run — use --apply to execute)");
                }
            }
        }
        Some(Command::Relocate {
            old_prefix,
            new_prefix,
            verify,
            pretend,
        }) => {
            let conn = db::open_database(config::paths::database_file())?;

            if verify {
                let missing = core::relocator::verify_paths(&conn)?;
                if missing.is_empty() {
                    println!("All {} track paths verified — no missing files.", {
                        let count: i64 =
                            conn.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?;
                        count
                    });
                } else {
                    println!("{} missing file(s):", missing.len());
                    for (id, path) in &missing {
                        println!("  [{}] {}", id, path);
                    }
                }
            } else if let (Some(old), Some(new)) = (old_prefix, new_prefix) {
                let old_str = old.display().to_string();
                let new_str = new.display().to_string();
                let plan = core::relocator::plan_relocate(&conn, &old_str, &new_str)?;

                if plan.is_empty() {
                    println!("No tracks match the prefix '{}'.", old_str);
                } else {
                    for (_, old_path, new_path) in &plan {
                        println!("  {} → {}", old_path, new_path);
                    }
                    println!("\n{} path(s) to update.", plan.len());

                    if pretend {
                        println!("(dry run — remove --pretend to apply)");
                    } else {
                        let count = core::relocator::apply_relocate(&conn, &plan)?;
                        println!("Done: {} path(s) updated.", count);
                    }
                }
            } else {
                eprintln!("Usage: kyoku relocate <old_prefix> <new_prefix>");
                eprintln!("       kyoku relocate --verify");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
