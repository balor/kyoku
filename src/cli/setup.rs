use std::path::PathBuf;

use inquire::{Confirm, Select, Text};

use crate::config::{
    self, Settings,
    settings::{CoverArtSize, NameScriptPreference},
};

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

    // Music directory. Loop until we have a usable path — exists (or the
    // user agreed to create it) and accepts writes. Bailing early here is
    // much friendlier than importing a few hundred files and then hitting
    // the first failed rename.
    let default_music = current.library.music_dir.display().to_string();
    let music_dir = loop {
        let raw = Text::new("Music directory:")
            .with_default(&default_music)
            .with_help_message("Root directory for your organized music library")
            .prompt()?;
        let expanded = config::paths::expand_tilde(&raw);

        // Not existing is the only recoverable failure — offer to mkdir -p.
        if !expanded.exists() {
            let create = Confirm::new(&format!(
                "{} does not exist. Create it?",
                expanded.display()
            ))
            .with_default(true)
            .prompt()?;
            if !create {
                println!("  → pick a different path.");
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&expanded) {
                println!("  Could not create directory: {}", e);
                continue;
            }
        }

        match config::paths::validate_library_dir(&expanded) {
            Ok(()) => break raw,
            Err(reason) => {
                println!("  {}", reason);
                println!("  → pick a different path.");
            }
        }
    };

    // Database directory. Same loop pattern as music_dir — the DB directory
    // is often kept next to the music itself (so DB + files travel together
    // on an external drive), so we have to actually validate it instead of
    // falling back to the platform default.
    let default_db_dir = current.library.data_dir.display().to_string();
    let default_db_dir = if default_db_dir.is_empty() {
        config::paths::default_data_dir().display().to_string()
    } else {
        default_db_dir
    };
    let db_dir = loop {
        let raw = Text::new("Database directory:")
            .with_default(&default_db_dir)
            .with_help_message(
                "Where kyoku stores library.db — defaults to the platform data dir, \
                 but it's common to point this at the music drive",
            )
            .prompt()?;
        let expanded = config::paths::expand_tilde(&raw);

        if !expanded.exists() {
            let create = Confirm::new(&format!(
                "{} does not exist. Create it?",
                expanded.display()
            ))
            .with_default(true)
            .prompt()?;
            if !create {
                println!("  → pick a different path.");
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&expanded) {
                println!("  Could not create directory: {}", e);
                continue;
            }
        }

        match config::paths::validate_library_dir(&expanded) {
            Ok(()) => break raw,
            Err(reason) => {
                println!("  {}", reason);
                println!("  → pick a different path.");
            }
        }
    };

    // Inbox directories
    println!();
    println!("Inbox directories are folders kyoku watches for new music to import.");
    println!("Common choices: ~/Downloads, ~/Music/Incoming");
    println!();

    let mut inbox_dirs: Vec<PathBuf> = Vec::new();
    for existing in &current.library.inbox_dirs {
        if validate_inbox_dir(existing).is_ok() {
            println!("Keeping existing inbox: {}", existing.display());
            inbox_dirs.push(existing.clone());
        } else {
            let prompt = format!(
                "Existing inbox {} is not usable. Keep it anyway?",
                existing.display()
            );
            if Confirm::new(&prompt).with_default(false).prompt()? {
                inbox_dirs.push(existing.clone());
            }
        }
    }

    // Offer any Nicotine+ download directories we can detect.
    for dir in detect_nicotine_download_dirs() {
        if inbox_dirs.iter().any(|p| p == &dir) {
            continue;
        }
        let prompt = format!(
            "Detected Nicotine+ download folder: {}  —  add as inbox?",
            dir.display()
        );
        if Confirm::new(&prompt).with_default(true).prompt()? {
            inbox_dirs.push(dir);
        }
    }

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

        let expanded = config::paths::expand_tilde(&dir);
        if !expanded.exists() {
            let create = Confirm::new(&format!(
                "{} does not exist. Create it?",
                expanded.display()
            ))
            .with_default(true)
            .prompt()?;
            if !create {
                println!("  → pick a different path.");
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&expanded) {
                println!("  Could not create directory: {}", e);
                continue;
            }
        }
        match validate_inbox_dir(&expanded) {
            Ok(()) => {
                if !inbox_dirs.iter().any(|p| p == &expanded) {
                    inbox_dirs.push(expanded);
                }
            }
            Err(reason) => {
                println!("  {}", reason);
                println!("  → pick a different path.");
            }
        }
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
    let name_script = match selected_script.split(' ').next().unwrap_or("native") {
        "latin" => NameScriptPreference::Latin,
        _ => NameScriptPreference::Native,
    };

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
    let cover_art_size = match selected_size.split_whitespace().next().unwrap_or("500") {
        "250" => CoverArtSize::Px250,
        "1200" => CoverArtSize::Px1200,
        "original" => CoverArtSize::Original,
        _ => CoverArtSize::Px500,
    };

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
    let theme = selected
        .split(' ')
        .next()
        .unwrap_or("tokyo-night")
        .to_string();

    // Preserve the existing settings and only overwrite fields this wizard
    // actually asked about. This keeps custom templates, thresholds,
    // write_tags, show_cover_preview, etc. intact on setup re-runs.
    let mut next = current.clone();
    next.library.music_dir = PathBuf::from(&music_dir);
    next.library.data_dir = PathBuf::from(&db_dir);
    next.library.inbox_dirs = inbox_dirs;
    next.musicbrainz.name_script = name_script;
    next.musicbrainz.cover_art_size = cover_art_size;
    next.ui.theme = theme;

    let config_content = render_config(&next)?;

    // Write config
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&config_path, config_content)?;

    let db_path = config::paths::expand_tilde(&next.library.data_dir);

    println!();
    println!("Config written to {}", config_path.display());
    println!("Database will be stored in {}", db_path.display());
    println!();
    println!("You're all set! Run `kyoku` to launch the TUI, or `kyoku --help` for commands.");

    Ok(())
}

fn validate_inbox_dir(path: &std::path::Path) -> std::result::Result<(), String> {
    if !path.exists() {
        return Err(format!("does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()));
    }
    Ok(())
}

fn render_config(settings: &Settings) -> anyhow::Result<String> {
    let body = toml::to_string_pretty(settings)?;
    Ok(format!(
        "# kyoku config.toml\n\
# Generated by `kyoku setup`. You can edit this file directly.\n\
#\n\
# Path templates are used by `kyoku organize`. Available variables include:\n\
#   {{artist}}, {{album_artist}}, {{album}}, {{year}}, {{track}}, {{title}},\n\
#   {{disc}}, {{genre}}, {{label}}, {{ext}}, and for collection templates\n\
#   {{collection}} plus {{position}}.\n\
#\n\
# MusicBrainz user-agent is compiled into kyoku; no config key is needed.\n\n{body}"
    ))
}

/// Look for a Nicotine+ config file and return any `downloaddir` paths that
/// exist on disk. Nicotine+ stores its config as INI-ish text under the
/// platform config dir (e.g. `~/.config/nicotine/config`).
fn detect_nicotine_download_dirs() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(config_dir) = dirs::config_dir() {
        candidates.push(config_dir.join("nicotine").join("config"));
    }
    if let Some(home) = dirs::home_dir() {
        // Linux-style fallback (useful on macOS where users sometimes point
        // Nicotine+ at `~/.config` instead of `~/Library/Application Support`).
        let linux_style = home.join(".config").join("nicotine").join("config");
        if !candidates.contains(&linux_style) {
            candidates.push(linux_style);
        }
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for cfg in candidates {
        let Ok(content) = std::fs::read_to_string(&cfg) else {
            continue;
        };
        let mut in_transfers = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(section) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                in_transfers = section.eq_ignore_ascii_case("transfers");
                continue;
            }
            if !in_transfers {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            if key.trim() != "downloaddir" {
                continue;
            }
            let value = value.trim().trim_matches(&['\'', '"'][..]);
            if value.is_empty() {
                continue;
            }
            let Some(expanded) = expand_path_vars(value) else {
                continue;
            };
            if expanded.is_dir() && !found.contains(&expanded) {
                found.push(expanded);
            }
        }
    }
    found
}

/// Expand `${VAR}`, `$VAR` and a leading `~` the way a Nicotine+ config
/// path tends to use them. Returns `None` if any referenced variable can't
/// be resolved (so we don't offer a broken path).
fn expand_path_vars(input: &str) -> Option<PathBuf> {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // ${VAR}
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                let end = bytes[i + 2..].iter().position(|&b| b == b'}')?;
                let name = &input[i + 2..i + 2 + end];
                out.push_str(&resolve_var(name)?);
                i += 2 + end + 1;
                continue;
            }
            // $VAR
            let rest = &input[i + 1..];
            let name_len = rest
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            if name_len > 0 {
                let name = &rest[..name_len];
                out.push_str(&resolve_var(name)?);
                i += 1 + name_len;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    // Leading `~` or `~/` → home dir.
    let path = if out == "~" {
        dirs::home_dir()?
    } else if let Some(rest) = out.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else {
        PathBuf::from(out)
    };
    Some(path)
}

/// Resolve a variable name found in a Nicotine+ config path. Falls back to
/// Nicotine+'s own defaults for `NICOTINE_DATA_HOME` / `NICOTINE_CONFIG_HOME`
/// when the env var isn't set, because Nicotine+ resolves those internally.
fn resolve_var(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    let home = dirs::home_dir()?;
    let path = match name {
        "NICOTINE_DATA_HOME" => std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("nicotine"))
            .unwrap_or_else(|| home.join(".local").join("share").join("nicotine")),
        "NICOTINE_CONFIG_HOME" => std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("nicotine"))
            .unwrap_or_else(|| home.join(".config").join("nicotine")),
        "XDG_DATA_HOME" => home.join(".local").join("share"),
        "XDG_CONFIG_HOME" => home.join(".config"),
        "HOME" => home,
        _ => return None,
    };
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_config_escapes_paths_and_omits_dead_user_agent() {
        let mut settings = Settings::default();
        settings.library.music_dir = PathBuf::from("/tmp/music with \"quote\"");
        settings.library.inbox_dirs = vec![PathBuf::from("/tmp/inbox\\slash")];
        settings.library.path_template = "custom/{artist}/{title}.{ext}".to_string();
        settings.import.auto_match_threshold = 0.91;
        settings.import.match_candidates = 9;
        settings.tagging.write_tags = false;
        settings.ui.show_cover_preview = false;

        let rendered = render_config(&settings).unwrap();

        assert!(!rendered.contains("user_agent"));
        assert!(rendered.contains("custom/{artist}/{title}.{ext}"));
        let parsed: Settings = toml::from_str(&rendered).unwrap();
        assert_eq!(parsed.library.music_dir, settings.library.music_dir);
        assert_eq!(parsed.library.inbox_dirs, settings.library.inbox_dirs);
        assert_eq!(parsed.import.auto_match_threshold, 0.91);
        assert_eq!(parsed.import.match_candidates, 9);
        assert!(!parsed.tagging.write_tags);
        assert!(!parsed.ui.show_cover_preview);
    }
}
