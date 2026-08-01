//! External music player hand-off.
//!
//! kyoku is not a music player — this module assembles an ordered file
//! list (album, collection, marked tracks, single track), packages it for
//! an external player, and launches that player. Design doc:
//! `doc/design/2026-07-19-external-player.md`.
//!
//! Transports:
//! - **Playlist** (default): items are written to a single UTF-8 M3U8 in
//!   the cache dir and the player gets that one path. A one-item play
//!   skips the playlist and opens the audio file directly.
//! - **FileList**: items are passed as argv (`player f1 f2 …` or a
//!   comma-joined `--enqueue-files=` arg) for players that can't load
//!   playlist files (Amberol, Quod Libet). Derived from the argv template:
//!   any `{files}`/`{files-csv}` placeholder implies FileList.
//!
//! Player resolution: `[player].command` → `[player].app` (macOS) →
//! per-platform auto-detect table → OS default handler. Detection runs
//! through the [`Probes`] trait so tests can fake a machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::Settings;
use crate::db::queries::{self, TrackRow};
use crate::error::{KyokuError, Result};

/// One playable item — a file plus the metadata needed for `#EXTINF`.
#[derive(Debug, Clone)]
pub struct PlayItem {
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
}

impl PlayItem {
    /// Build a minimal item from a bare path (CLI `kyoku play <path>`).
    /// Title falls back to the file stem.
    pub fn from_path(path: PathBuf) -> Self {
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        PlayItem {
            path,
            title,
            artist: None,
            duration_ms: None,
        }
    }
}

/// How the resolved player receives the file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Hand over one path: the generated .m3u8, or the audio file itself
    /// for a one-item play.
    Playlist,
    /// Hand over the files themselves as argv entries.
    FileList,
}

/// A chosen player, ready to receive a target.
#[derive(Debug, Clone)]
pub struct ResolvedPlayer {
    /// Short human name for notices ("mpv", "IINA", "custom (mpv)").
    pub label: String,
    /// Argv template. May contain `{playlist}`, `{files}`, `{files-csv}`.
    pub argv: Vec<String>,
    pub transport: Transport,
}

/// Everything needed to launch (or dry-run) a play action.
#[derive(Debug, Clone)]
pub struct PlayOutcome {
    pub player_label: String,
    pub argv: Vec<String>,
    /// Where the M3U8 was written (playlist transport only).
    pub playlist_path: Option<PathBuf>,
    pub played: usize,
    /// Items dropped because the file is missing on disk (or unwritable
    /// in the transport — a literal newline in a path breaks M3U).
    pub skipped_missing: usize,
}

/// What gets substituted into the argv template.
#[derive(Debug, Clone)]
enum PlayTarget {
    /// One audio file, opened directly (no playlist written).
    SingleFile(PathBuf),
    /// Path to the generated .m3u8.
    Playlist(PathBuf),
    /// Files passed as argv.
    FileList(Vec<PathBuf>),
}

// ── Items builders ──────────────────────────────────────────────────

/// Map track rows to play items. When `collection_paths` is given, a
/// track whose recorded collection copy exists on disk plays *that copy*
/// (same rule as the collection-detail footer badge); otherwise the
/// primary `file_path` is used.
pub fn items_from_rows(
    rows: &[TrackRow],
    collection_paths: Option<&HashMap<i64, String>>,
) -> Vec<PlayItem> {
    rows.iter()
        .map(|t| {
            let path = collection_paths
                .and_then(|cp| cp.get(&t.id))
                .filter(|p| Path::new(p.as_str()).exists())
                .cloned()
                .unwrap_or_else(|| t.file_path.clone());
            PlayItem {
                path: PathBuf::from(path),
                title: t.title.clone(),
                artist: t.artist.clone(),
                duration_ms: t.duration_ms,
            }
        })
        .collect()
}

/// All tracks of an album in disc/track order.
pub fn album_items(conn: &Connection, music_dir: &Path, album_id: i64) -> Result<Vec<PlayItem>> {
    let rows = queries::get_album_tracks(conn, music_dir, album_id)?;
    Ok(items_from_rows(&rows, None))
}

/// All tracks of a collection in effective collection order (mixtape
/// order *is* the point), preferring organized collection copies.
pub fn collection_items(
    conn: &Connection,
    music_dir: &Path,
    collection_id: i64,
) -> Result<Vec<PlayItem>> {
    // No real cap — the limit here is "all of them". (A negative LIMIT
    // would also mean "unlimited" in SQLite, but an explicit large
    // constant is clearer than relying on that.)
    let rows = queries::get_collection_tracks(conn, music_dir, collection_id, 0, 1_000_000)?;
    let copies =
        queries::get_collection_file_paths(conn, music_dir, collection_id).unwrap_or_default();
    Ok(items_from_rows(&rows, Some(&copies)))
}

// ── Playlist writer ─────────────────────────────────────────────────

/// Fixed playlist filename — overwritten on every multi-file play, so
/// the cache never grows. Never deleted after spawn: mpv & friends read
/// playlist files lazily, so the file must outlive the spawn.
pub const PLAYLIST_FILENAME: &str = "kyoku-play.m3u8";

/// Write items as UTF-8 M3U8 (EXTM3U + EXTINF) under
/// `<cache_dir>/play/`. Returns the playlist path.
pub fn write_playlist(items: &[PlayItem], cache_dir: &Path) -> Result<PathBuf> {
    let dir = cache_dir.join("play");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(PLAYLIST_FILENAME);

    let mut out = String::from("#EXTM3U\n");
    for item in items {
        // Round to nearest second; unknown duration → -1 per M3U convention.
        let secs = item
            .duration_ms
            .map(|ms| (ms + 500) / 1000)
            .map(|s| s as i64)
            .unwrap_or(-1);
        let display = match &item.artist {
            Some(a) if !a.is_empty() => format!("{} - {}", a, item.title),
            _ => item.title.clone(),
        };
        // EXTINF is a single-line field — strip stray newlines defensively.
        let display = display.replace(['\n', '\r'], " ");
        out.push_str(&format!(
            "#EXTINF:{},{}\n{}\n",
            secs,
            display,
            item.path.display()
        ));
    }
    std::fs::write(&path, out)?;
    Ok(path)
}

// ── Player detection ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOS,
    Windows,
}

/// Platform probes behind a trait so tests can fake a machine.
pub trait Probes {
    /// Binary found on PATH and executable.
    fn which(&self, bin: &str) -> bool;
    /// `<app>.app` present in /Applications or ~/Applications.
    fn app_exists(&self, app: &str) -> bool;
    /// Windows only: `<ProgramFiles* >\<rel>` exists as a file; returns
    /// the absolute path to use as argv[0]. Never called for bins/apps —
    /// and never on Unix (the default keeps fakes tiny).
    fn program_file(&self, rel: &str) -> Option<PathBuf> {
        let _ = rel;
        None
    }
    fn os(&self) -> Os;
}

pub struct RealProbes;

impl Probes for RealProbes {
    fn which(&self, bin: &str) -> bool {
        let Some(path_var) = std::env::var_os("PATH") else {
            return false;
        };
        let names = bin_candidates(bin);
        std::env::split_paths(&path_var).any(|dir| {
            names.iter().any(|n| {
                let p = dir.join(n);
                p.is_file() && is_executable(&p)
            })
        })
    }

    fn program_file(&self, rel: &str) -> Option<PathBuf> {
        windows_program_file(rel)
    }

    fn app_exists(&self, app: &str) -> bool {
        let bundle = format!("{}.app", app);
        if Path::new("/Applications").join(&bundle).is_dir() {
            return true;
        }
        dirs::home_dir()
            .map(|h| h.join("Applications").join(&bundle).is_dir())
            .unwrap_or(false)
    }

    fn os(&self) -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOS
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// Names to probe on PATH. Windows stores binaries with an extension
/// from `%PATHEXT%` (`mpv.exe`, `rg.bat`, …), so a bare `"mpv"` probe
/// would never match; Unix stores the bare name.
fn bin_candidates(bin: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        pathext_candidates(bin, std::env::var("PATHEXT").ok().as_deref())
    }
    #[cfg(not(windows))]
    {
        vec![bin.to_string()]
    }
}

/// Expand `bin` with every extension from a PATHEXT-style value. Pure
/// and target-independent so the Windows lookup shape is tested on any
/// host (NTFS is case-insensitive, so lowercased extensions still hit).
#[cfg(any(windows, test))]
fn pathext_candidates(bin: &str, pathext: Option<&str>) -> Vec<String> {
    // Already carries an extension — probe it as-is.
    if Path::new(bin).extension().is_some() {
        return vec![bin.to_string()];
    }
    let mut out = vec![bin.to_string()];
    let pathext = pathext.unwrap_or(".COM;.EXE;.BAT;.CMD");
    for ext in pathext.split(';') {
        let ext = ext.trim();
        if !ext.is_empty() {
            out.push(format!("{bin}{}", ext.to_lowercase()));
        }
    }
    out
}

/// Probe the well-known Windows install roots (%ProgramFiles%,
/// %ProgramFiles(x86)%, %ProgramW6432%) for a relative exe path.
/// Most popular Windows players never touch PATH, so PATH-probing
/// alone would miss them.
#[cfg(windows)]
fn windows_program_file(rel: &str) -> Option<PathBuf> {
    ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|root| Path::new(&root).join(rel))
        .find(|p| p.is_file())
}

/// Unix never probes Windows install dirs.
#[cfg(not(windows))]
fn windows_program_file(_rel: &str) -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_p: &Path) -> bool {
    true
}

struct DetectEntry {
    label: &'static str,
    bin: Option<&'static str>,
    app: Option<&'static str>,
    /// Windows install-dir probe: path relative to %ProgramFiles% /
    /// %ProgramFiles(x86)%. On hit, `{exe}` in argv is substituted with
    /// the resolved absolute path.
    program_files: Option<&'static str>,
    argv: &'static [&'static str],
}

/// Linux auto-detect table, first hit wins. Syntax verified against man
/// pages / app docs (see design doc §13 research log).
const LINUX_PLAYERS: &[DetectEntry] = &[
    DetectEntry {
        label: "mpv",
        bin: Some("mpv"),
        app: None,
        program_files: None,
        argv: &["mpv", "--playlist={playlist}"],
    },
    DetectEntry {
        label: "VLC",
        bin: Some("vlc"),
        app: None,
        program_files: None,
        argv: &["vlc", "{playlist}"],
    },
    DetectEntry {
        label: "Celluloid",
        bin: Some("celluloid"),
        app: None,
        program_files: None,
        argv: &["celluloid", "{playlist}"],
    },
    DetectEntry {
        label: "Haruna",
        bin: Some("haruna"),
        app: None,
        program_files: None,
        argv: &["haruna", "{playlist}"],
    },
    DetectEntry {
        label: "Strawberry",
        bin: Some("strawberry"),
        app: None,
        program_files: None,
        argv: &["strawberry", "--load", "{playlist}"],
    },
    DetectEntry {
        label: "Clementine",
        bin: Some("clementine"),
        app: None,
        program_files: None,
        argv: &["clementine", "--load", "{playlist}"],
    },
    DetectEntry {
        label: "Audacious",
        bin: Some("audacious"),
        app: None,
        program_files: None,
        argv: &["audacious", "-E", "{playlist}"],
    },
    DetectEntry {
        label: "DeaDBeeF",
        bin: Some("deadbeef"),
        app: None,
        program_files: None,
        argv: &["deadbeef", "{playlist}"],
    },
    // File-list transport entries (no playlist-file support).
    DetectEntry {
        label: "Amberol",
        bin: Some("amberol"),
        app: None,
        program_files: None,
        argv: &["amberol", "{files}"],
    },
    DetectEntry {
        label: "Quod Libet",
        bin: Some("quodlibet"),
        app: None,
        program_files: None,
        argv: &["quodlibet", "--enqueue-files={files-csv}"],
    },
];

/// macOS auto-detect table: PATH binaries first, then app bundles in
/// /Applications|~/Applications. Music is deliberately last — its
/// default "Copy files to Media folder when adding to library" setting
/// can duplicate the user's files (Apple Support mus3081).
const MAC_PLAYERS: &[DetectEntry] = &[
    DetectEntry {
        label: "mpv",
        bin: Some("mpv"),
        app: None,
        program_files: None,
        argv: &["mpv", "--playlist={playlist}"],
    },
    DetectEntry {
        label: "IINA",
        bin: None,
        app: Some("IINA"),
        program_files: None,
        argv: &["open", "-a", "IINA", "{playlist}"],
    },
    DetectEntry {
        label: "VLC",
        bin: None,
        app: Some("VLC"),
        program_files: None,
        argv: &["open", "-a", "VLC", "{playlist}"],
    },
    DetectEntry {
        label: "foobar2000",
        bin: None,
        app: Some("foobar2000"),
        program_files: None,
        argv: &["open", "-a", "foobar2000", "{playlist}"],
    },
    DetectEntry {
        label: "Swinsian",
        bin: None,
        app: Some("Swinsian"),
        program_files: None,
        argv: &["open", "-a", "Swinsian", "{playlist}"],
    },
    DetectEntry {
        label: "Cog",
        bin: None,
        app: Some("Cog"),
        program_files: None,
        argv: &["open", "-a", "Cog", "{playlist}"],
    },
    DetectEntry {
        label: "Music",
        bin: None,
        app: Some("Music"),
        program_files: None,
        argv: &["open", "-a", "Music", "{playlist}"],
    },
];

/// Windows auto-detect table, first hit wins. PATH binaries first
/// (scoop/chocolatey installs put mpv on PATH; stock installers don't),
/// then well-known install dirs under %ProgramFiles% — that's where the
/// bulk of popular Windows players live. Verified against each player's
/// CLI docs: fb2k loads .m3u8 via a plain path arg; VLC likewise.
const WINDOWS_PLAYERS: &[DetectEntry] = &[
    DetectEntry {
        label: "mpv",
        bin: Some("mpv"),
        app: None,
        program_files: None,
        argv: &["mpv", "--playlist={playlist}"],
    },
    DetectEntry {
        label: "VLC",
        bin: Some("vlc"),
        app: None,
        program_files: None,
        argv: &["vlc", "{playlist}"],
    },
    DetectEntry {
        label: "foobar2000",
        bin: None,
        app: None,
        program_files: Some("foobar2000\\foobar2000.exe"),
        argv: &["{exe}", "{playlist}"],
    },
    DetectEntry {
        label: "VLC",
        bin: None,
        app: None,
        program_files: Some("VideoLAN\\VLC\\vlc.exe"),
        argv: &["{exe}", "{playlist}"],
    },
];

/// Infer the transport from an argv template: any `{files}`/`{files-csv}`
/// placeholder means the player receives the files themselves.
fn transport_for(argv: &[String]) -> Transport {
    if argv
        .iter()
        .any(|a| a.contains("{files-csv}") || a.contains("{files}"))
    {
        Transport::FileList
    } else {
        Transport::Playlist
    }
}

fn mac_app_entry(app: &str) -> ResolvedPlayer {
    let argv = vec![
        "open".to_string(),
        "-a".to_string(),
        app.to_string(),
        "{playlist}".to_string(),
    ];
    ResolvedPlayer {
        label: app.to_string(),
        argv,
        transport: Transport::Playlist,
    }
}

/// Resolve which player to use. Never fails: the last step is the OS
/// default handler (`xdg-open`/`open`), which always exists as an argv.
pub fn resolve_player(settings: &Settings, probes: &dyn Probes) -> ResolvedPlayer {
    // 1. Explicit argv template wins.
    if let Some(cmd) = settings.player.command.as_ref().filter(|c| !c.is_empty()) {
        return ResolvedPlayer {
            label: format!("custom ({})", cmd[0]),
            transport: transport_for(cmd),
            argv: cmd.clone(),
        };
    }

    // 2. macOS-only configured app.
    if probes.os() == Os::MacOS
        && let Some(app) = settings
            .player
            .app
            .as_ref()
            .filter(|a| !a.trim().is_empty())
    {
        return mac_app_entry(app.trim());
    }

    // 3. Auto-detect table for the platform.
    let table: &[DetectEntry] = match probes.os() {
        Os::Linux => LINUX_PLAYERS,
        Os::MacOS => MAC_PLAYERS,
        Os::Windows => WINDOWS_PLAYERS,
    };
    for entry in table {
        // Outer Option = detected at all; inner = resolved exe path for
        // install-dir probes (substituted for `{exe}` in argv).
        let exe: Option<Option<PathBuf>> = match (entry.bin, entry.app, entry.program_files) {
            (Some(bin), _, _) => probes.which(bin).then_some(None),
            (None, Some(app), _) => probes.app_exists(app).then_some(None),
            (None, None, Some(rel)) => probes.program_file(rel).map(Some),
            _ => None,
        };
        if let Some(exe) = exe {
            let argv: Vec<String> = entry
                .argv
                .iter()
                .map(|s| match &exe {
                    Some(e) => s.replace("{exe}", &e.display().to_string()),
                    None => s.to_string(),
                })
                .collect();
            return ResolvedPlayer {
                label: entry.label.to_string(),
                transport: transport_for(&argv),
                argv,
            };
        }
    }

    // 4. OS default handler.
    // Windows: `explorer.exe <target>` opens the Shell association (works
    // for .m3u8 too). Chosen over `cmd /c start ""` deliberately: it's
    // spawn-success-detectable, doesn't flash a console window, and dodges
    // `start`'s first-quoted-arg-is-a-title quoting trap.
    match probes.os() {
        Os::Linux => ResolvedPlayer {
            label: "xdg-open (default handler)".to_string(),
            argv: vec!["xdg-open".to_string(), "{playlist}".to_string()],
            transport: Transport::Playlist,
        },
        Os::MacOS => ResolvedPlayer {
            label: "open (default handler)".to_string(),
            argv: vec!["open".to_string(), "{playlist}".to_string()],
            transport: Transport::Playlist,
        },
        Os::Windows => ResolvedPlayer {
            label: "explorer (default handler)".to_string(),
            argv: vec!["explorer.exe".to_string(), "{playlist}".to_string()],
            transport: Transport::Playlist,
        },
    }
}

// ── Invocation building & spawn ─────────────────────────────────────

fn render_argv(template: &[String], target: &PlayTarget) -> Vec<String> {
    let mut out = Vec::new();
    let mut placeholder_used = false;
    for arg in template {
        if arg.contains("{files-csv}") {
            placeholder_used = true;
            if let PlayTarget::FileList(files) = target {
                let csv = files
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                out.push(arg.replace("{files-csv}", &csv));
            }
        } else if arg.contains("{files}") {
            placeholder_used = true;
            if let PlayTarget::FileList(files) = target {
                for f in files {
                    out.push(arg.replace("{files}", &f.display().to_string()));
                }
            }
        } else if arg.contains("{playlist}") {
            placeholder_used = true;
            let p = match target {
                PlayTarget::SingleFile(p) | PlayTarget::Playlist(p) => p,
                PlayTarget::FileList(_) => continue,
            };
            out.push(arg.replace("{playlist}", &p.display().to_string()));
        } else {
            out.push(arg.clone());
        }
    }
    if !placeholder_used {
        match target {
            PlayTarget::SingleFile(p) | PlayTarget::Playlist(p) => {
                out.push(p.display().to_string())
            }
            PlayTarget::FileList(files) => {
                out.extend(files.iter().map(|f| f.display().to_string()))
            }
        }
    }
    out
}

/// Split items into playable (file exists, path transport-safe) and
/// skipped. A literal newline breaks the M3U line format; a comma breaks
/// the CSV flavor of the file-list transport.
fn filter_playable(items: Vec<PlayItem>, csv: bool) -> (Vec<PlayItem>, usize) {
    let mut playable = Vec::new();
    let mut skipped = 0usize;
    for item in items {
        let s = item.path.display().to_string();
        let bad = !item.path.exists() || s.contains('\n') || (csv && s.contains(','));
        if bad {
            skipped += 1;
        } else {
            playable.push(item);
        }
    }
    (playable, skipped)
}

/// Assemble everything for a play action short of spawning. Public for
/// `kyoku play --dry-run`; tests use [`prepare_with_probes`].
pub fn prepare(settings: &Settings, items: Vec<PlayItem>) -> Result<PlayOutcome> {
    prepare_with_probes(settings, items, &RealProbes)
}

pub fn prepare_with_probes(
    settings: &Settings,
    items: Vec<PlayItem>,
    probes: &dyn Probes,
) -> Result<PlayOutcome> {
    let player = resolve_player(settings, probes);
    let csv = player.argv.iter().any(|a| a.contains("{files-csv}"));
    let (playable, skipped_missing) = filter_playable(items, csv);
    if playable.is_empty() {
        return Err(KyokuError::External(format!(
            "nothing playable — {} file(s) missing on disk",
            skipped_missing
        )));
    }

    let cache_dir = crate::config::paths::cache_dir();
    let (target, playlist_path) = match player.transport {
        Transport::Playlist if playable.len() == 1 => {
            (PlayTarget::SingleFile(playable[0].path.clone()), None)
        }
        Transport::Playlist => {
            let p = write_playlist(&playable, &cache_dir)?;
            (PlayTarget::Playlist(p.clone()), Some(p))
        }
        Transport::FileList => (
            PlayTarget::FileList(playable.iter().map(|i| i.path.clone()).collect()),
            None,
        ),
    };

    let argv = render_argv(&player.argv, &target);
    Ok(PlayOutcome {
        player_label: player.label,
        argv,
        playlist_path,
        played: playable.len(),
        skipped_missing,
    })
}

/// Launch the resolved player. Fire-and-forget: the child must outlive
/// kyoku and never block the UI thread (same discipline as
/// `open_directory` in the detail view).
pub fn play(settings: &Settings, items: Vec<PlayItem>) -> Result<PlayOutcome> {
    let outcome = prepare(settings, items)?;
    spawn_argv(&outcome.argv)?;
    Ok(outcome)
}

fn spawn_argv(argv: &[String]) -> Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(KyokuError::External(
            "no player resolved — set [player] command in config".to_string(),
        ));
    };
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            KyokuError::External(format!(
                "failed to launch `{}`: {} — install it or set [player] command in config",
                program, e
            ))
        })
}

/// Format the per-view notice for a successful play. `context` is what
/// the user played ("幻燈", "3 albums", "12 tracks").
pub fn outcome_notice(outcome: &PlayOutcome, context: &str) -> String {
    let mut msg = format!(
        "Playing: {} ({} track{}) via {}",
        context,
        outcome.played,
        if outcome.played == 1 { "" } else { "s" },
        outcome.player_label,
    );
    if outcome.skipped_missing > 0 {
        msg.push_str(&format!(
            " — {} skipped (files missing)",
            outcome.skipped_missing
        ));
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── Fake probes ─────────────────────────────────────────────────

    struct FakeProbes {
        bins: HashSet<&'static str>,
        apps: HashSet<&'static str>,
        pf: HashSet<&'static str>,
        os: Os,
    }

    impl FakeProbes {
        fn linux(bins: &[&'static str]) -> Self {
            Self {
                bins: bins.iter().cloned().collect(),
                apps: HashSet::new(),
                pf: HashSet::new(),
                os: Os::Linux,
            }
        }
        fn mac(apps: &[&'static str]) -> Self {
            Self {
                bins: HashSet::new(),
                apps: apps.iter().cloned().collect(),
                pf: HashSet::new(),
                os: Os::MacOS,
            }
        }
        fn windows(bins: &[&'static str], program_files: &[&'static str]) -> Self {
            Self {
                bins: bins.iter().cloned().collect(),
                apps: HashSet::new(),
                pf: program_files.iter().cloned().collect(),
                os: Os::Windows,
            }
        }
    }

    impl Probes for FakeProbes {
        fn which(&self, bin: &str) -> bool {
            self.bins.contains(bin)
        }
        fn app_exists(&self, app: &str) -> bool {
            self.apps.contains(app)
        }
        fn program_file(&self, rel: &str) -> Option<PathBuf> {
            self.pf
                .contains(rel)
                .then(|| PathBuf::from(format!("C:\\Program Files\\{rel}")))
        }
        fn os(&self) -> Os {
            self.os
        }
    }

    fn item(path: &Path, title: &str) -> PlayItem {
        PlayItem {
            path: path.to_path_buf(),
            title: title.to_string(),
            artist: Some("Artist".to_string()),
            duration_ms: Some(180_000),
        }
    }

    fn settings_no_player() -> Settings {
        Settings::default()
    }

    // ── Resolution ──────────────────────────────────────────────────

    #[test]
    fn configured_command_wins_over_detection() {
        let mut s = settings_no_player();
        s.player.command = Some(vec!["myplayer".into(), "--flag".into()]);
        let probes = FakeProbes::linux(&["mpv"]);
        let r = resolve_player(&s, &probes);
        assert_eq!(r.argv, vec!["myplayer", "--flag"]);
        assert_eq!(r.transport, Transport::Playlist);
        assert!(r.label.contains("custom"));
    }

    #[test]
    fn empty_command_falls_through_to_detection() {
        let mut s = settings_no_player();
        s.player.command = Some(vec![]);
        let probes = FakeProbes::linux(&["mpv"]);
        let r = resolve_player(&s, &probes);
        assert_eq!(r.label, "mpv");
    }

    #[test]
    fn configured_app_wins_on_macos() {
        let mut s = settings_no_player();
        s.player.app = Some("MyPlayer".into());
        let probes = FakeProbes::mac(&["IINA"]);
        let r = resolve_player(&s, &probes);
        assert_eq!(r.argv, vec!["open", "-a", "MyPlayer", "{playlist}"]);
    }

    #[test]
    fn linux_detection_order_and_syntax() {
        let probes = FakeProbes::linux(&["strawberry", "mpv"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "mpv", "mpv outranks strawberry");

        let probes = FakeProbes::linux(&["strawberry", "audacious"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.argv, vec!["strawberry", "--load", "{playlist}"]);

        let probes = FakeProbes::linux(&["audacious"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.argv, vec!["audacious", "-E", "{playlist}"]);
    }

    #[test]
    fn file_list_transport_derived_from_template() {
        let probes = FakeProbes::linux(&["amberol"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "Amberol");
        assert_eq!(r.transport, Transport::FileList);

        let probes = FakeProbes::linux(&["quodlibet"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.transport, Transport::FileList);
    }

    #[test]
    fn mac_detection_iina_beats_music_and_music_is_last() {
        let probes = FakeProbes::mac(&["IINA", "Music"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "IINA");

        let probes = FakeProbes::mac(&["Music"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "Music", "Music present but only as last resort");
    }

    #[test]
    fn fallback_is_os_default_handler() {
        let r = resolve_player(&settings_no_player(), &FakeProbes::linux(&[]));
        assert_eq!(r.argv, vec!["xdg-open", "{playlist}"]);

        let r = resolve_player(&settings_no_player(), &FakeProbes::mac(&[]));
        assert_eq!(r.argv, vec!["open", "{playlist}"]);

        let r = resolve_player(&settings_no_player(), &FakeProbes::windows(&[], &[]));
        assert_eq!(r.argv, vec!["explorer.exe", "{playlist}"]);
    }

    // ── Windows detection ───────────────────────────────────────────

    #[test]
    fn windows_path_binaries_detected_first() {
        let probes = FakeProbes::windows(&["mpv"], &["foobar2000\\foobar2000.exe"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "mpv");
        assert_eq!(r.argv, vec!["mpv", "--playlist={playlist}"]);

        let probes = FakeProbes::windows(&["vlc"], &["foobar2000\\foobar2000.exe"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "VLC", "stock PATH install beats install-dir fb2k");
        assert_eq!(r.argv, vec!["vlc", "{playlist}"]);
    }

    #[test]
    fn windows_program_files_probe_resolves_absolute_exe() {
        let probes = FakeProbes::windows(&[], &["foobar2000\\foobar2000.exe"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.label, "foobar2000");
        // argv[0] is the resolved absolute path — spawn can't rely on PATH
        // for install-dir players.
        assert_eq!(
            r.argv,
            vec![
                "C:\\Program Files\\foobar2000\\foobar2000.exe",
                "{playlist}"
            ]
        );
        assert_eq!(r.transport, Transport::Playlist);

        // VLC's Program Files variant works the same way when not on PATH.
        let probes = FakeProbes::windows(&[], &["VideoLAN\\VLC\\vlc.exe"]);
        let r = resolve_player(&settings_no_player(), &probes);
        assert_eq!(r.argv[0], "C:\\Program Files\\VideoLAN\\VLC\\vlc.exe");
    }

    #[test]
    fn windows_mpv_outranks_fb2k_and_vlc_pf_is_last() {
        let probes = FakeProbes::windows(
            &["mpv"],
            &["foobar2000\\foobar2000.exe", "VideoLAN\\VLC\\vlc.exe"],
        );
        assert_eq!(resolve_player(&settings_no_player(), &probes).label, "mpv");

        let probes = FakeProbes::windows(&[], &["VideoLAN\\VLC\\vlc.exe"]);
        assert_eq!(resolve_player(&settings_no_player(), &probes).label, "VLC");
    }

    // ── PATHEXT expansion (pure shape, tested on every host) ────────

    #[test]
    fn pathext_candidates_expands_bare_name() {
        let got = pathext_candidates("mpv", Some(".COM;.EXE;.BAT;.CMD;.VBS"));
        assert_eq!(
            got,
            vec!["mpv", "mpv.com", "mpv.exe", "mpv.bat", "mpv.cmd", "mpv.vbs"]
        );
    }

    #[test]
    fn pathext_candidates_keeps_extension_and_handles_defaults() {
        assert_eq!(pathext_candidates("mpv.exe", Some(".EXE")), vec!["mpv.exe"]);
        // Missing/empty PATHEXT falls back to sane defaults.
        let got = pathext_candidates("mpv", None);
        assert!(got.contains(&"mpv.exe".to_string()), "{got:?}");
        let got = pathext_candidates("mpv", Some(";.EXE;;"));
        assert_eq!(got, vec!["mpv", "mpv.exe"]);
    }

    // ── Argv rendering ──────────────────────────────────────────────

    // Path display follows the host separator, so expectations use the
    // shared plat helpers (see core::paths::plat) — a unix literal like
    // "/a.flac" displays as "\a.flac" on Windows.
    use crate::core::paths::plat::{abs, abs_str};

    #[test]
    fn argv_playlist_placeholder_substituted() {
        let tpl = vec!["mpv".to_string(), "--playlist={playlist}".to_string()];
        let argv = render_argv(&tpl, &PlayTarget::Playlist(abs("/tmp/x.m3u8")));
        assert_eq!(
            argv,
            vec![
                "mpv".to_string(),
                format!("--playlist={}", abs_str("/tmp/x.m3u8"))
            ]
        );
    }

    #[test]
    fn argv_no_placeholder_appends_target() {
        let tpl = vec!["vlc".to_string()];
        let argv = render_argv(&tpl, &PlayTarget::SingleFile(abs("/a.flac")));
        assert_eq!(argv, vec!["vlc".to_string(), abs_str("/a.flac")]);

        let tpl = vec!["amberol".to_string()];
        let argv = render_argv(&tpl, &PlayTarget::FileList(vec![abs("/a"), abs("/b")]));
        assert_eq!(
            argv,
            vec!["amberol".to_string(), abs_str("/a"), abs_str("/b")]
        );
    }

    #[test]
    fn argv_files_and_files_csv_expansion() {
        let tpl = vec!["amberol".to_string(), "{files}".to_string()];
        let argv = render_argv(&tpl, &PlayTarget::FileList(vec![abs("/a"), abs("/b")]));
        assert_eq!(
            argv,
            vec!["amberol".to_string(), abs_str("/a"), abs_str("/b")]
        );

        let tpl = vec![
            "quodlibet".to_string(),
            "--enqueue-files={files-csv}".to_string(),
        ];
        let argv = render_argv(&tpl, &PlayTarget::FileList(vec![abs("/a"), abs("/b")]));
        assert_eq!(
            argv,
            vec![
                "quodlibet".to_string(),
                format!("--enqueue-files={},{}", abs_str("/a"), abs_str("/b"))
            ]
        );
    }

    // ── Playlist writer ─────────────────────────────────────────────

    #[test]
    fn playlist_is_utf8_m3u8_with_extinf() {
        let tmp = tempfile::tempdir().unwrap();
        let items = vec![
            PlayItem {
                path: abs("/music/ヨルシカ/靴の花火.flac"),
                title: "靴の花火".to_string(),
                artist: Some("ヨルシカ".to_string()),
                duration_ms: Some(183_400),
            },
            PlayItem {
                path: abs("/music/x.mp3"),
                title: "NoDuration".to_string(),
                artist: None,
                duration_ms: None,
            },
        ];
        let path = write_playlist(&items, tmp.path()).unwrap();
        assert!(path.ends_with("kyoku-play.m3u8"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("#EXTM3U\n"));
        let expected_a = abs_str("/music/ヨルシカ/靴の花火.flac");
        let expected_b = abs_str("/music/x.mp3");
        assert!(content.contains(&format!("#EXTINF:183,ヨルシカ - 靴の花火\n{expected_a}\n")));
        assert!(content.contains(&format!("#EXTINF:-1,NoDuration\n{expected_b}\n")));
    }

    // ── prepare / filter ────────────────────────────────────────────

    #[test]
    fn prepare_single_item_opens_file_directly_without_playlist() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = tmp.path().join("song.flac");
        std::fs::write(&audio, b"x").unwrap();
        let mut s = settings_no_player();
        s.player.command = Some(vec!["fake-player".into()]);
        let outcome = prepare(&s, vec![item(&audio, "Song")]).unwrap();
        assert_eq!(outcome.played, 1);
        assert_eq!(outcome.playlist_path, None);
        let expected = audio.display().to_string();
        assert_eq!(outcome.argv, vec!["fake-player", expected.as_str()]);
    }

    #[test]
    fn prepare_multi_item_writes_playlist_and_counts_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.flac");
        let b = tmp.path().join("b.flac");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let missing = tmp.path().join("gone.flac");
        let mut s = settings_no_player();
        s.player.command = Some(vec!["fake-player".into()]);
        let items = vec![item(&a, "A"), item(&missing, "Gone"), item(&b, "B")];
        let outcome = prepare(&s, items).unwrap();
        assert_eq!(outcome.played, 2);
        assert_eq!(outcome.skipped_missing, 1);
        let playlist = outcome.playlist_path.expect("multi-item → playlist");
        let content = std::fs::read_to_string(&playlist).unwrap();
        assert!(content.contains(a.display().to_string().as_str()));
        assert!(content.contains(b.display().to_string().as_str()));
        assert!(!content.contains("gone.flac"));
    }

    #[test]
    fn prepare_all_missing_is_an_error() {
        let s = settings_no_player();
        let items = vec![item(Path::new("/definitely/not/here.flac"), "X")];
        let err = prepare(&s, items).unwrap_err().to_string();
        assert!(err.contains("nothing playable"), "{err}");
    }

    #[test]
    fn prepare_csv_transport_skips_comma_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let weird = tmp.path().join("a,b.flac");
        let ok = tmp.path().join("ok.flac");
        std::fs::write(&weird, b"x").unwrap();
        std::fs::write(&ok, b"x").unwrap();
        let mut s = settings_no_player();
        s.player.command = Some(vec![
            "quodlibet".into(),
            "--enqueue-files={files-csv}".into(),
        ]);
        let outcome = prepare(&s, vec![item(&weird, "W"), item(&ok, "OK")]).unwrap();
        assert_eq!(outcome.played, 1);
        assert_eq!(outcome.skipped_missing, 1);
        assert_eq!(outcome.argv.len(), 2);
        assert!(outcome.argv[1].starts_with("--enqueue-files="));
        assert!(!outcome.argv[1].contains("a,b.flac"));
    }

    // ── Items builders (DB-backed) ──────────────────────────────────

    fn seed_track(
        conn: &Connection,
        music_dir: &Path,
        path: &Path,
        title: &str,
        n: u32,
        album_id: Option<i64>,
    ) -> i64 {
        let track = crate::db::models::Track {
            id: None,
            album_id,
            title: title.to_string(),
            artist: Some("Artist".to_string()),
            track_number: Some(n),
            disc_number: 1,
            duration_ms: Some(1000),
            mbid: None,
            file_path: path.to_path_buf(),
            file_format: crate::db::models::AudioFormat::Flac,
            bitrate: None,
            sample_rate: None,
            tag_status: crate::db::models::TagStatus::Unmatched,
            source_dir: None,
        };
        queries::insert_track(conn, music_dir, &track, album_id, None).unwrap()
    }

    #[test]
    fn album_items_follow_disc_track_order() {
        let tmp = tempfile::tempdir().unwrap();
        let music = tmp.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        let conn = crate::db::open_memory().unwrap();
        let (aid, _) =
            queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 3).unwrap();
        // Insert out of order — builder must return disc/track order.
        let p3 = music.join("03.flac");
        std::fs::write(&p3, b"x").unwrap();
        let p1 = music.join("01.flac");
        std::fs::write(&p1, b"x").unwrap();
        let p2 = music.join("02.flac");
        std::fs::write(&p2, b"x").unwrap();
        seed_track(&conn, &music, &p3, "Three", 3, Some(aid));
        seed_track(&conn, &music, &p1, "One", 1, Some(aid));
        seed_track(&conn, &music, &p2, "Two", 2, Some(aid));

        let items = album_items(&conn, &music, aid).unwrap();
        assert_eq!(
            items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            vec!["One", "Two", "Three"]
        );
        assert_eq!(items[0].path, p1);
    }

    #[test]
    fn collection_items_prefer_existing_copy_fall_back_to_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let music = tmp.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        let conn = crate::db::open_memory().unwrap();
        let (cid, _) = queries::get_or_create_collection(&conn, "Mix").unwrap();

        // Track A: collection copy exists on disk → plays the copy.
        let a_primary = music.join("a.flac");
        std::fs::write(&a_primary, b"x").unwrap();
        let a_copy = music.join("Collections/Mix/a.flac");
        std::fs::create_dir_all(a_copy.parent().unwrap()).unwrap();
        std::fs::write(&a_copy, b"x").unwrap();
        let a = seed_track(&conn, &music, &a_primary, "A", 1, None);

        // Track B: copy recorded but gone from disk → falls back to primary.
        let b_primary = music.join("b.flac");
        std::fs::write(&b_primary, b"x").unwrap();
        let b_copy = music.join("Collections/Mix/b.flac");
        let b = seed_track(&conn, &music, &b_primary, "B", 2, None);

        queries::add_tracks_to_collection_ordered(&conn, cid, &[a, b]).unwrap();
        queries::update_collection_track_path(&conn, &music, cid, a, &a_copy.display().to_string())
            .unwrap();
        queries::update_collection_track_path(&conn, &music, cid, b, &b_copy.display().to_string())
            .unwrap();

        let items = collection_items(&conn, &music, cid).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, a_copy, "existing copy wins");
        assert_eq!(items[1].path, b_primary, "missing copy → primary");
    }

    #[test]
    fn notice_mentions_context_counts_and_player() {
        let outcome = PlayOutcome {
            player_label: "mpv".to_string(),
            argv: vec![],
            playlist_path: None,
            played: 12,
            skipped_missing: 2,
        };
        let n = outcome_notice(&outcome, "幻燈");
        assert!(
            n.contains("幻燈")
                && n.contains("12 tracks")
                && n.contains("mpv")
                && n.contains("2 skipped"),
            "{n}"
        );
    }
}
