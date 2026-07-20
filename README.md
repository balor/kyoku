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

- **External player hand-off** — play an album, a collection, a track, or a marked selection with one key (`p`/`P`) or with `kyoku play`. Auto-detects mpv, VLC, Celluloid, Haruna, Strawberry, Clementine, Audacious, DeaDBeeF, Amberol, Quod Libet on Linux and IINA, VLC, foobar2000, Swinsian, Cog, Music on macOS; falls back to the OS default handler.
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

To pin a specific music player for `p`/`kyoku play` (otherwise auto-detected):

```toml
[player]
# argv template; {playlist} is replaced with the generated .m3u8 path
command = ["mpv", "--playlist={playlist}"]
# or, on macOS only, an app name launched via `open -a`:
# app = "IINA"
```

---

## Tech stack

Rust 2024 · ratatui + crossterm (TUI) · rusqlite bundled (SQLite) · lofty (tag I/O) · reqwest + rustls (HTTP) · strsim (fuzzy matching) · walkdir · inquire (setup prompts). Zero system dependencies; the same source compiles on macOS and Linux without conditional code.

---

## License

MIT — see [LICENSE](LICENSE).
