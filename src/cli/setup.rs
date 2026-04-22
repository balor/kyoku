use std::path::PathBuf;

use inquire::{Confirm, Select, Text};

use crate::config::{self, Settings, settings::{CoverArtSize, NameScriptPreference}};

pub fn run(current: Settings) -> anyhow::Result<()> {
    let config_path = config::paths::config_file();

    println!("kyoku setup");
    println!("===========");
    println!();
    println!(
        "This will create (or overwrite) your config file at:\n  {}",
        config_path.display()
    );
    println!();
    println!(
        "The config controls where kyoku looks for music, where it stores\n\
         its database, and how it organizes your library. You can always\n\
         edit the file directly later."
    );
    println!();

    if config_path.exists() {
        let overwrite = Confirm::new("Config file already exists. Overwrite?")
            .with_default(false)
            .prompt()?;
        if !overwrite {
            println!("Setup cancelled.");
            return Ok(());
        }
        println!();
    }

    // Music directory
    let default_music = current.library.music_dir.display().to_string();
    let music_dir = Text::new("Music directory:")
        .with_default(&default_music)
        .with_help_message("Root directory for your organized music library")
        .prompt()?;

    // Database directory
    let default_db_dir = config::paths::data_dir().display().to_string();
    let db_dir = Text::new("Database directory:")
        .with_default(&default_db_dir)
        .with_help_message("Where kyoku stores library.db (leave default unless you have a reason)")
        .prompt()?;

    // Inbox directories
    println!();
    println!("Inbox directories are folders kyoku watches for new music to import.");
    println!("Common choices: ~/Downloads, ~/Music/Incoming");
    println!();

    let mut inbox_dirs: Vec<String> = Vec::new();
    loop {
        let prompt = if inbox_dirs.is_empty() {
            "Add an inbox directory (or press Enter to skip):"
        } else {
            "Add another inbox directory (or press Enter to finish):"
        };

        let dir = Text::new(prompt).with_placeholder("~/Downloads").prompt()?;

        if dir.trim().is_empty() {
            break;
        }
        inbox_dirs.push(dir);
    }

    // Name script preference
    println!();
    println!("Script preference for artist & album names from MusicBrainz.");
    println!("Affects what gets written to tags, DB, and filenames when MB");
    println!("returns both a native-script name and a Latin-script alias");
    println!("(e.g. ヨルシカ vs Yorushika, 花冷え。 vs HANABIE.).");
    println!("Track titles are never remapped. Falls back to canonical when");
    println!("no alias matches.");
    println!();
    let scripts = vec![
        "native — use MB's canonical name (default)",
        "latin  — prefer Latin-script alias when available",
    ];
    let script_default_idx = match current.musicbrainz.name_script {
        NameScriptPreference::Native => 0,
        NameScriptPreference::Latin => 1,
    };
    let selected_script = Select::new("Name script:", scripts)
        .with_starting_cursor(script_default_idx)
        .prompt()?;
    let name_script = selected_script.split(' ').next().unwrap_or("native");

    // Cover Art Archive download size
    println!();
    println!("Download size for album covers fetched from the Cover Art Archive");
    println!("(the `C` key in album detail). Larger = better quality but more");
    println!("disk space and slower downloads. Cover is also rendered in the TUI;");
    println!("the TUI itself doesn't benefit much past 500 px, but a larger file");
    println!("helps if you also feed your library to a media server or web app.");
    println!();
    let sizes = vec![
        "250      — ~20 KB, throwaway thumbnail",
        "500      — ~80 KB, default; fine for TUI",
        "1200     — ~300 KB, sweet spot for media servers",
        "original — full upload (1-8 MB), falls back to 1200 then 500",
    ];
    let size_default_idx = match current.musicbrainz.cover_art_size {
        CoverArtSize::Px250 => 0,
        CoverArtSize::Px500 => 1,
        CoverArtSize::Px1200 => 2,
        CoverArtSize::Original => 3,
    };
    let selected_size = Select::new("Cover art size:", sizes)
        .with_starting_cursor(size_default_idx)
        .prompt()?;
    let cover_art_size = selected_size.split_whitespace().next().unwrap_or("500");

    // Theme
    println!();
    let themes = vec![
        "tokyo-night (dark)",
        "tokyo-night-light (light)",
        "kanagawa (dark)",
        "kanagawa-lotus (light)",
    ];
    let default_idx = themes
        .iter()
        .position(|t| t.starts_with(&current.ui.theme))
        .unwrap_or(0);
    let selected = Select::new("Theme:", themes)
        .with_starting_cursor(default_idx)
        .prompt()?;
    let theme = selected.split(' ').next().unwrap_or("tokyo-night");

    // Build config
    let inbox_toml = if inbox_dirs.is_empty() {
        "[]".to_string()
    } else {
        let entries: Vec<String> = inbox_dirs
            .iter()
            .map(|d| format!("    \"{}\"", d))
            .collect();
        format!("[\n{},\n]", entries.join(",\n"))
    };

    let config_content = format!(
        r#"[library]
# Root directory for managed music files
music_dir = "{music_dir}"

# Inbox directories — kyoku scans these for new/unimported files.
inbox_dirs = {inbox_toml}

# Path template for organizing files (used by `kyoku organize`)
# Available variables: {{artist}}, {{album_artist}}, {{album}}, {{year}}, {{track}},
#                      {{title}}, {{disc}}, {{genre}}, {{ext}}, {{artist_sort}}
path_template = "{{album_artist}}/{{album}} ({{year}})/{{disc:0}}-{{track:02}} {{title}}.{{ext}}"

# Template for single-disc albums (disc_total == 1)
path_template_single_disc = "{{album_artist}}/{{album}} ({{year}})/{{track:02}} {{title}}.{{ext}}"

[import]
# Options: "move" (move to music_dir), "copy" (copy, keep originals)
organize_operation = "move"

# Auto-accept MusicBrainz matches above this similarity (0.0 - 1.0)
auto_match_threshold = 0.95

# Use AcoustID fingerprinting for matching (requires network)
use_fingerprint = true

# Number of MusicBrainz match candidates to display
match_candidates = 5

# Skip files already in the library
skip_duplicates = true

[tagging]
# Write tags back to files (if false, only updates DB)
write_tags = true

[musicbrainz]
user_agent = "kyoku/0.1.0 (https://github.com/yourname/kyoku)"
rate_limit_ms = 1100
# "native" keeps MB canonical names; "latin" prefers romanised alias when present.
name_script = "{name_script}"
# Cover Art Archive download size for the `C` fetch in album detail.
# Options:
#   "250"      — tiny thumbnail, ~20 KB
#   "500"      — default, ~80 KB, plenty for the TUI preview
#   "1200"     — ~300 KB, good if you also use a media server / web UI
#   "original" — uploader's full image (often 1-8 MB);
#                falls back to 1200 then 500 when no original is archived
cover_art_size = "{cover_art_size}"

[acoustid]
# Get a key at https://acoustid.org/new-application
api_key = ""

[ui]
# Dark: "tokyo-night", "kanagawa"
# Light: "tokyo-night-light", "kanagawa-lotus"
theme = "{theme}"
"#,
    );

    // Write config
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&config_path, config_content)?;

    // Ensure data dir exists
    let db_path = PathBuf::from(&db_dir);
    std::fs::create_dir_all(&db_path)?;

    println!();
    println!("Config written to {}", config_path.display());
    println!("Database will be stored in {}", db_path.display());
    println!();
    println!("You're all set! Run `kyoku` to launch the TUI, or `kyoku --help` for commands.");

    Ok(())
}
