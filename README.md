# kyoku (曲)

A TUI-first music library manager written in Rust.

The job: turn a messy pile of audio files into a **beautifully organized directory tree** that any file-browser-based music player can navigate directly. The filesystem is the product. The SQLite database is an index, not the source of truth.

kyoku reads, catalogs, and enriches metadata. You decide when and how files move. Every destructive operation has a dry-run preview and explicit confirmation.

> Personal project, no stable-release plans. It's the tool I wanted for myself; it'll keep evolving as I use it.

---

## Why this exists

[beets](https://beets.io/) is an impressive project with deep capabilities, and most people who care about music cataloging should probably just use it. I'm not a power-user music cataloguer though, and a few things consistently got in my way:

1. **I want search to feel like `ripgrep`, not like SQL.** A single freeform query should work 90% of the time; structured filters stay available, but never required.
2. **Loose collections as first-class objects.** Mixtapes, DJ sets, a folder of random MP3s, personal compilations, soundtracks with 40 different artists — I want these to live alongside the album hierarchy, not get squeezed into it.
3. **A TUI I can drive without the docs open.** Every screen shows its keys at the bottom; browsing the library, tweaking tags, or running an import wizard should be discoverable by poking around, not by memorising a query DSL.
4. **CJK / Unicode working everywhere.** Japanese, Korean, Chinese characters in tags, filenames, search, display, and sorting — handled as a default, not a config tweak.
5. **Library relocation should be a non-event.** Paths inside the library are stored relative to `music_dir`, so renaming the directory (or moving the whole library to another drive, as long as the DB travels with it) just needs the config to point at the new path — no DB rebase, no migration step.

None of these are damning critiques of beets — it's a different design point and an older, much richer tool. kyoku is just a smaller, narrower thing shaped to one person's habits.

---

## What kyoku isn't

- Not a music player — no *built-in* playback; kyoku hands files to your external player (`p` in the TUI, `kyoku play` in the CLI).
- Not a streaming client.
- Not a recommendation engine.
- Not a web app.
- Not a native Windows GUI (runs on Windows via WSL).
- Not automatic — kyoku will never move or rename files without an explicit action from you.

---

## Features

- **Import wizard** with MusicBrainz matching, manual MBID lookup, and per-group decisions (accept, skip, import-loose, assign to a collection).
- **Duplicate detection** against the library and within the batch — surfaced as side-by-side "keep A vs. B" decisions before anything touches disk.
- **Library browser** with sortable album list, cover-art previews, album detail, and theme support.
- **Collections** — loose, non-album groupings that live alongside the album hierarchy.
- **Tag editor** that writes both the DB and the file tags.
- **Cover art fetch** from the Cover Art Archive.
- **Organize** with templated paths and a dry-run preview.
- **Relative media paths** for music and cover art under `music_dir`, so DB-next-to-media libraries can move without rebasing.
- **Scriptable CLI** for common automation (`import`, `scan`, `organize`, `info`, `paths`, `setup`).
- **Preview-first file operations** for moving, copying, deleting, and reorganizing music.
- **External player hand-off** — play an album, a collection, a track, or a marked selection with one key (`p`/`P`) or with `kyoku play`. Auto-detects mpv, VLC, Celluloid, Haruna, Strawberry, Clementine, Audacious, DeaDBeeF, Amberol, Quod Libet on Linux and IINA, VLC, foobar2000, Swinsian, Cog, Music on macOS; falls back to the OS default handler.

---

## Keep your own backup

kyoku previews and confirms every destructive operation, but it does move and rewrite files on your behalf — and like any software, it has bugs. **Always keep a separate, independent copy of your music collection** (cloud sync, an external drive, a NAS snapshot — whatever fits) before pointing kyoku at anything you can't afford to lose. Shit happens.

---

## Install

### Prebuilt binaries

Download the binary for your platform from the repository's Releases page and put `kyoku` somewhere on your `PATH`.

### Build from source

```sh
cargo install --path .
# or for a local build
cargo build --release
./target/release/kyoku --help
```

Requires a recent stable Rust toolchain (2024 edition). No system libraries required — rusqlite is bundled; TLS goes through rustls.

### macOS binary note

The macOS release binaries are not Apple-notarized, so Gatekeeper will block them on first run with "Apple could not verify…". Strip the quarantine attribute after extracting:

```sh
xattr -d com.apple.quarantine ./kyoku
```

Or use System Settings → Privacy & Security → "Open Anyway".

---

## Quick start

```sh
kyoku setup       # interactive first-run wizard
kyoku             # launch the TUI — everything else lives in there
```

The CLI subcommands (`import`, `scan`, `organize`, …) are there for scripting, but day-to-day use happens mostly in the TUI. MusicBrainz review/matching currently lives in the TUI import wizard; CLI import is a simpler as-is cataloging path. Run `kyoku --help` if you want to see the commands.

Config lives at `$XDG_CONFIG_HOME/kyoku/config.toml`, or `~/.config/kyoku/config.toml` if that variable isn't set. The database location is configurable during setup. Run `kyoku paths` to see the exact locations on your machine.

To pin a specific music player for `p`/`kyoku play` (otherwise auto-detected), set `[player] command` or, on macOS only, `[player] app` — see [External player hand-off](#external-player-hand-off) for the full auto-detect tables per platform.

---

## Tech stack

Rust 2024 · ratatui + crossterm (TUI) · rusqlite bundled (SQLite) · lofty (tag I/O) · reqwest + rustls (HTTP) · strsim (fuzzy matching) · walkdir · inquire (setup prompts). Zero system dependencies; the same source compiles on macOS and Linux without conditional code.

---

## External player hand-off

kyoku is not a player. `p`/`P` in the TUI and `kyoku play` in the CLI hand your selection off to an external player. Player resolution order (see `src/core/player.rs`):

1. **`[player].command`** — an explicit argv template wins. Placeholders: `{playlist}` (path to the generated `.m3u8`, or the single audio file for a one-track play), `{files}` (expands to one argv item per file), `{files-csv}` (comma-joined, for Quod Libet-style players). With no placeholder, the target path(s) are appended as trailing args.
2. **`[player].app`** (macOS only) — an app bundle launched via `open -a <app> <playlist>`. Ignored on other platforms.
3. **Auto-detect table** — platform-specific, first match wins. PATH binaries are found via `which`; macOS app bundles via `.app` in `/Applications` or `~/Applications`.
4. **OS default handler** — never fails: `xdg-open {playlist}` on Linux, `open {playlist}` on macOS.

All default players use the **Playlist** transport (a UTF-8 M3U8 is written to the cache dir as `kyoku-play.m3u8`); Amberol and Quod Libet instead receive the files directly as argv (**FileList** transport). A single-item play always opens the audio file directly — no playlist is written.

### Linux (first match wins)

| # | Player | Binary | Argv | Transport |
|---|--------|--------|------|-----------|
| 1 | mpv | `mpv` | `mpv --playlist={playlist}` | Playlist |
| 2 | VLC | `vlc` | `vlc {playlist}` | Playlist |
| 3 | Celluloid | `celluloid` | `celluloid {playlist}` | Playlist |
| 4 | Haruna | `haruna` | `haruna {playlist}` | Playlist |
| 5 | Strawberry | `strawberry` | `strawberry --load {playlist}` | Playlist |
| 6 | Clementine | `clementine` | `clementine --load {playlist}` | Playlist |
| 7 | Audacious | `audacious` | `audacious -E {playlist}` | Playlist |
| 8 | DeaDBeeF | `deadbeef` | `deadbeef {playlist}` | Playlist |
| 9 | Amberol | `amberol` | `amberol {files}` | FileList |
| 10 | Quod Libet | `quodlibet` | `quodlibet --enqueue-files={files-csv}` | FileList |
| — | (fallback) | — | `xdg-open {playlist}` | Playlist |

### macOS (first match wins)

| # | Player | Detection | Argv | Transport |
|---|--------|-----------|------|-----------|
| 1 | mpv | PATH binary `mpv` | `mpv --playlist={playlist}` | Playlist |
| 2 | IINA | `IINA.app` | `open -a IINA {playlist}` | Playlist |
| 3 | VLC | `VLC.app` | `open -a VLC {playlist}` | Playlist |
| 4 | foobar2000 | `foobar2000.app` | `open -a foobar2000 {playlist}` | Playlist |
| 5 | Swinsian | `Swinsian.app` | `open -a Swinsian {playlist}` | Playlist |
| 6 | Cog | `Cog.app` | `open -a Cog {playlist}` | Playlist |
| 7 | Music | `Music.app` | `open -a Music {playlist}` | Playlist |
| — | (fallback) | — | `open {playlist}` | Playlist |

> **macOS note:** mpv is the only macOS entry that checks a PATH binary (it ships as a CLI tool, often via Homebrew); the rest are `.app` bundles launched through `open -a`. **Music.app** is deliberately last — its "Copy files to Media folder when adding to library" setting can duplicate your files (Apple Support mus3081), so kyoku prefers an existing copy-aware player first.

### Configuration

```toml
[player]
# Full argv template (both platforms):
command = ["mpv", "--playlist={playlist}"]
# macOS-only app name launched via `open -a` (ignored on Linux):
# app = "IINA"
```

Leave both fields unset to use auto-detection.

---

## License

MIT — see [LICENSE](LICENSE).
