# kyoku (曲) — Music Library Manager

> A TUI-first music library manager written in Rust.
> Curates a beautiful, browsable filesystem that any music player can navigate — because the file tree IS the product.

> **Status note:** this spec is a living design/roadmap document. The README and source code describe the currently shipped behavior; sections marked future/roadmap are intentional plans, not implemented features.

---

## 1. Vision & Goals

### What kyoku Is
A **TUI-first** application for managing a local music library. Running `kyoku` launches the interactive terminal UI. CLI subcommands exist for scripting and automation, not as the primary interface.

The core job: turn a messy pile of audio files into a **beautifully organized directory tree** that any file-browser-based music player (foobar2000, Deadbeef, Strawberry, mpd, Navidrome) can navigate directly. The filesystem is the product. The SQLite database is an index, not the source of truth.

The library state is persisted in SQLite, and **all file operations (rename, move, reorganize) are deliberate user actions** — never automatic side effects of import. kyoku reads, catalogs, and enriches metadata. You decide when and how files move.

**Target library scale**: 20,000–100,000 tracks. All queries, scans, and TUI interactions must remain responsive at this scale. Use pagination, lazy loading, and indexed queries accordingly.

### What kyoku Is Not (v1)
- Not a music player (no playback)
- Not a streaming service client
- Not a recommendation engine (planned for v2 via OpenAI-compatible API)
- Not a web application
- Not a native Windows GUI app (native Windows runs are terminal-based, like everywhere else)

### Platform Support
- **macOS** — primary development target
- **Linux** — full support (native)
- **Windows** — full support (native, Windows 10/11; Windows Terminal or WezTerm recommended; legacy conhost works minus cover previews). WSL continues to work but is no longer the recommended path.

Windows specifics (implemented 2026-08; see `doc/design/2026-08-01-windows-support.md`):
- Player auto-detect probes PATH (PATHEXT-aware) and `%ProgramFiles%` install dirs; default handler is `explorer.exe`.
- Path sanitization additionally dodges reserved device names (`NUL`, `CON`, `AUX`, `COM1–9`, `LPT1–9`) on all platforms, so libraries stay NTFS-movable.
- Collision detection in organize/delete is case-folded on Windows (NTFS is case-insensitive by default).
- Windows denies move/rename/delete of files held open by players; per-file errors surface in organize/delete results.
- The release exe embeds a `longPathAware` manifest; >260-char paths additionally require the user's `LongPathsEnabled` registry/GPO opt-in.

The current Rust stack (`ratatui`/`crossterm`, `rusqlite` bundled, `lofty`, `reqwest` + rustls) has zero required system libraries and compiles on macOS and Linux. Crossterm handles terminal abstraction. Filesystem paths use `std::path::Path` throughout (never hardcoded separators).

### Problems with Beets (Why This Exists)

These are the specific frustrations this project aims to solve:

1. **Search is clunky and complex.** Beets' query language (`beet ls artist::^Radio album:OK`) is powerful but hostile to casual use. kyoku should feel like `ripgrep` for music: a single freeform query works 90% of the time, structured filters available when needed but never required.

2. **Custom compilations and loose collections are a nightmare.** Beets enforces a strict Artist → Album → Track hierarchy. Real music libraries have mixtapes, DJ sets, random MP3s in a folder, personal compilations, soundtracks with 40 different artists. kyoku treats "a folder of loosely related files" as a first-class concept, not a pathological edge case.

3. **Niche content with poor metadata service coverage.** Doujin music, indie netlabels, Bandcamp-only releases, field recordings — MusicBrainz simply doesn't have entries for much of this. Beets pushes hard toward "everything must match." kyoku treats unmatched content as normal citizens, not second-class items needing to be fixed.

4. **Poor Unicode / CJK handling.** Japanese, Chinese, Korean characters in tags, filenames, and search. Beets has recurring issues with encoding, display, sorting, and filesystem operations for non-Latin content. kyoku must handle CJK as a core concern, not an afterthought.

5. **Library relocation is painful.** Moving a library to a new drive or path in beets historically required manual SQLite queries. kyoku stores file paths relative to `music_dir` (the approach beets adopted in v2.10 — see [beets#133](https://github.com/beetbox/beets/issues/133)), so renaming the library directory (or moving it wholesale to a new drive when the DB lives alongside) is a config edit and nothing more — no rebase, no migration, no `relocate` subcommand to remember.

### Design Principles
1. **TUI-first** — the TUI is the primary interface; CLI subcommands are for automation and scripting
2. **The filesystem is the product** — the real output is a beautifully organized directory tree, not a database; any music player with a file browser should love your library
3. **File operations are deliberate** — import reads and catalogs; rename/move/organize are separate, explicit actions the user triggers; you always have the last word
4. **Safety first** — never modify files without explicit confirmation; always show a dry-run diff
5. **Offline-capable** — core library management works without network; MusicBrainz lookups are optional enrichment
6. **Composable** — CLI subcommands work in pipelines (`kyoku import ~/Music/new --loose | ...`)
7. **Incremental** — can import 10 files or 100,000; designed for large libraries from the start
8. **Unmatched content is normal** — files without MusicBrainz matches are fully functional library members, not errors to be resolved
9. **Unicode-native** — CJK characters, diacritics, and mixed-script content work correctly in tags, filenames, search, display, and sorting throughout the entire pipeline
10. **Flexible organization** — support strict album hierarchies and loose collections equally well

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                      User Interface                      │
│  ┌──────────────┐  ┌──────────────────────────────────┐  │
│  │   CLI (clap)  │  │         TUI (ratatui)            │  │
│  └──────┬───────┘  └──────────────┬───────────────────┘  │
│         │                         │                       │
│         └────────────┬────────────┘                       │
│                      ▼                                    │
│              ┌───────────────┐                            │
│              │  Application  │  Commands, state machine   │
│              │     Core      │  Business logic             │
│              └───────┬───────┘                            │
│                      │                                    │
│         ┌────────────┼────────────┐                       │
│         ▼            ▼            ▼                       │
│  ┌────────────┐ ┌──────────┐ ┌──────────────┐           │
│  │  Library DB │ │ Tag I/O  │ │  External    │           │
│  │  (SQLite)   │ │ (lofty)  │ │  Services    │           │
│  └────────────┘ └──────────┘ │  - MusicBrainz│           │
│                               │  - Cover Art  │           │
│                               │  - future APIs│           │
│                               └──────────────┘           │
└─────────────────────────────────────────────────────────┘
```

---

## 3. Technology Stack

### Core Dependencies

| Crate | Purpose | Version / Notes |
|-------|---------|-----------------|
| `ratatui` / `crossterm` | TUI framework + terminal backend | Interactive UI and keyboard/event loop |
| `ratatui-image` / `image` | Cover previews | TUI album-art rendering without requiring system `libchafa` |
| `clap` | CLI argument parsing | v4, derive API |
| `lofty` | Audio metadata read/write | Multi-format tag I/O |
| `rusqlite` | SQLite database | With `bundled` feature for zero system deps |
| `reqwest` / `serde_json` | HTTP + JSON | Blocking client with rustls for MusicBrainz and Cover Art Archive |
| `serde` / `toml` | Configuration parsing | TOML config files |
| `thiserror` / `anyhow` | Error handling | `thiserror` for library errors, `anyhow` in main/CLI |
| `strsim` | Match scoring | Jaro-Winkler similarity for MusicBrainz candidate ranking |
| `walkdir` | Recursive directory scanning | Fast filesystem traversal |
| `dirs` | Directory resolution | Config/data/cache paths |
| `inquire` | Interactive CLI prompts | Setup wizard |
| `unicode-width` | TUI display widths | Correct table/layout widths for CJK and mixed-width text |
| `tracing` / `tracing-subscriber` | Logging | CLI/TUI-safe progress and diagnostics |

### Why These Choices

- **lofty over id3**: `lofty` handles all major audio formats with a unified API. `id3` is MP3-only. A music library manager must handle FLAC, OGG, M4A, etc.
- **rusqlite over sqlx**: Simpler setup, no need for compile-time query checking for this use case, `bundled` feature means zero external deps.
- **reqwest blocking client**: keeps network integration simple; MusicBrainz calls run from worker threads where needed so the TUI stays responsive.
- **ratatui + crossterm**: the dominant, actively maintained Rust TUI stack with portable terminal input/output.
- **ratatui-image without `chafa-dyn`**: avoids a system libchafa dependency while still supporting common terminal image protocols / fallbacks.

---

## 4. Data Model

### SQLite Schema

```sql
-- Core entities
CREATE TABLE IF NOT EXISTS albums (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    title           TEXT NOT NULL,
    album_artist    TEXT,
    year            INTEGER,
    mbid            TEXT UNIQUE,        -- MusicBrainz release MBID (the specific edition we matched)
    disc_total      INTEGER DEFAULT 1,
    track_total     INTEGER,
    genre           TEXT,               -- Primary genre
    label           TEXT,
    media_type      TEXT,               -- CD, Vinyl, Digital, etc.
    album_type      TEXT DEFAULT 'album', -- album, compilation, single, ep, soundtrack, live, other
    -- Cover art: path to folder art (cover.jpg, folder.png, etc.) or NULL if only embedded.
    -- Does not store the image itself — just a reference. Embedded art is read from files on demand.
    cover_art_path  TEXT,
    created_at      TEXT DEFAULT (datetime('now')),
    updated_at      TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS tracks (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id        INTEGER REFERENCES albums(id),  -- NULL for loose/ungrouped tracks
    title           TEXT NOT NULL,
    artist          TEXT,               -- Track artist (may differ from album artist)
    track_number    INTEGER,
    disc_number     INTEGER DEFAULT 1,
    duration_ms     INTEGER,            -- Duration in milliseconds
    mbid            TEXT,               -- MusicBrainz recording ID (nullable, not unique — niche content won't have one)
    
    -- File info
    file_path       TEXT NOT NULL UNIQUE,
    file_size       INTEGER,
    file_format     TEXT,               -- mp3, flac, ogg, m4a, etc.
    bitrate         INTEGER,            -- kbps
    sample_rate     INTEGER,            -- Hz
    channels        INTEGER,
    
    -- Original filesystem context (preserved on import for loose collections)
    source_dir      TEXT,               -- Original parent directory path
    
    -- Fingerprint
    acoustid        TEXT,               -- AcoustID fingerprint
    chromaprint     TEXT,               -- Raw chromaprint (for local dedup)
    
    -- Metadata state
    tag_status      TEXT DEFAULT 'unmatched',  -- unmatched, matched, verified, manual
    import_date     TEXT DEFAULT (datetime('now')),
    modified_date   TEXT DEFAULT (datetime('now'))
);

-- Collections: user-defined groupings that exist alongside the album hierarchy.
-- A collection is NOT an album — it's a loose bag: a folder of random MP3s, a DJ set,
-- a personal mixtape, "stuff I need to sort later." Tracks can be in zero, one, or many
-- collections AND also belong to an album. Collections don't affect tags;
-- they can affect collection-copy filenames via path templates and position.
CREATE TABLE IF NOT EXISTS collections (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    description TEXT,
    -- Path template override (optional). If unset, tracks in this collection
    -- use [library].collection_path_template when organized.
    -- e.g. "Collections/{collection}/{position:02} {artist} - {title}.{ext}"
    path_template TEXT,
    created_at  TEXT DEFAULT (datetime('now')),
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS collection_tracks (
    collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    track_id      INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    position      INTEGER,             -- Durable ordering within collection
    -- When the collection has a path_template, organize creates a physical copy
    -- of the file in the collection folder. This field tracks that copy's path.
    -- NULL if collection has no template or file hasn't been organized yet.
    collection_file_path TEXT,
    added_at      TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (collection_id, track_id)
);

-- Files whose DB track row has been removed (most often because import-time
-- duplicate resolution replaced them) but whose physical file still sits on
-- disk awaiting cleanup. The next `kyoku organize` run unlinks each file and
-- clears the tracking row. Snapshot fields preserve identifying tags so the
-- organize preview can show a human-readable label even though the track row
-- is gone.
CREATE TABLE IF NOT EXISTS orphaned_files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path   TEXT NOT NULL UNIQUE,
    title       TEXT,
    artist      TEXT,
    album_title TEXT,
    reason      TEXT NOT NULL,          -- e.g. "replaced by duplicate during import"
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Full-text search with CJK-aware tokenization.
-- unicode61 with remove_diacritics handles accented Latin characters (ą→a, ö→o).
-- For CJK, we additionally store a normalized form for substring matching.
-- Note: FTS5 unicode61 tokenizer handles CJK characters as individual tokens
-- (each character = one token), which works well for Chinese/Japanese search.
CREATE VIRTUAL TABLE IF NOT EXISTS tracks_fts USING fts5(
    title, artist, album_title,
    content='tracks',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album_id);
CREATE INDEX IF NOT EXISTS idx_tracks_path ON tracks(file_path);
CREATE INDEX IF NOT EXISTS idx_tracks_loose ON tracks(album_id) WHERE album_id IS NULL;
CREATE INDEX IF NOT EXISTS idx_albums_year ON albums(year);
CREATE INDEX IF NOT EXISTS idx_albums_type ON albums(album_type);
CREATE INDEX IF NOT EXISTS idx_tracks_status ON tracks(tag_status);
CREATE INDEX IF NOT EXISTS idx_collection_tracks_coll ON collection_tracks(collection_id);
CREATE INDEX IF NOT EXISTS idx_collection_tracks_track ON collection_tracks(track_id);
```

---

## 5. Configuration

### Location

Config deliberately uses an XDG-style path on every platform:

| | Linux / WSL | macOS | Windows |
|---|---|---|---|
| Config | `~/.config/kyoku/config.toml` | `~/.config/kyoku/config.toml` | `%USERPROFILE%\.config\kyoku\config.toml` |
| Database default | `~/.local/share/kyoku/library.db` | `~/Library/Application Support/kyoku/library.db` | `%APPDATA%\kyoku\library.db` |
| Cache | `~/.cache/kyoku/` | `~/Library/Caches/kyoku/` | `%LOCALAPPDATA%\kyoku\` |

`$XDG_CONFIG_HOME` overrides the config root. The database location is controlled by `[library] data_dir`; its default comes from the platform data directory. Cache uses the platform cache directory.

### config.toml

```toml
[library]
# Root directory for managed music files
music_dir = "~/Music"

# Directory holding library.db. Override this to keep DB + music together
# on an external drive.
data_dir = "~/.local/share/kyoku"

# Inbox directories — kyoku scans these for new/unimported files.
inbox_dirs = [
    "~/Downloads",
    "~/Music/Incoming",
]

# Path template for organizing album files (used by `kyoku organize`).
# Available variables: {artist}, {album_artist}, {album}, {year}, {track},
#                      {title}, {disc}, {genre}, {label}, {ext}
# Use {track:02} for zero-padded track numbers.
path_template = "{album_artist}/{album} ({year})/{disc:0}-{track:02} {title}.{ext}"

# Template for single-disc albums (disc_total == 1)
path_template_single_disc = "{album_artist}/{album} ({year})/{track:02} {title}.{ext}"

# Default template for collection copies. Same variables as album templates,
# plus {collection} and {position}. In collection templates, prefer {position}
# for numbering; {track} always means the file's track-number tag.
collection_path_template = "Collections/{collection}/{position:02} {album_artist} - {title}.{ext}"

# Template for loose tracks with no album and no collection.
loose_path_template = "_loose/{artist} - {title}.{ext}"

[import]
# This controls what happens when you explicitly organize:
# Options: "move" (move to music_dir), "copy" (copy, keep originals)
organize_operation = "move"

# Automatically accept MusicBrainz matches at or above this threshold (0.0 - 1.0)
auto_match_threshold = 0.85

# Number of MusicBrainz match candidates fetched per group
match_candidates = 5

[tagging]
# Write tags back to files (if false, only updates DB)
write_tags = true

[musicbrainz]
# Rate limiting (MB requires max 1 req/sec)
rate_limit_ms = 1100

# Preferred script for artist & album names written from MusicBrainz matches.
#   "native" — use MB's canonical credit/title (default)
#   "latin"  — prefer Latin-script alias (e.g. "Yorushika" over "ヨルシカ")
# Track titles are never remapped. Falls back to canonical when no alias matches.
name_script = "native"

# Cover Art Archive download size for the `C` fetch in album detail.
# Options: "250", "500", "1200", "original"
cover_art_size = "500"

[ui]
# TUI color scheme. 4 built-in themes.
# Dark:  "tokyo-night", "kanagawa"
# Light: "tokyo-night-light", "kanagawa-lotus"
theme = "tokyo-night"

# Render album cover previews in album detail.
show_cover_preview = true

# [ai]  # Future v2
# enabled = false
# endpoint = "http://localhost:11434/v1"  # OpenAI-compatible API
# model = "qwen3.5"
# features = ["recommendations", "duplicate_detection", "genre_classification"]
```

### Theme System

kyoku ships with 4 built-in themes — 2 dark, 2 light. Each theme defines a consistent palette used across all TUI elements (borders, tables, highlights, status badges, diffs, search).

#### Built-in Themes

| Theme | Variant | Character |
|-------|---------|-----------|
| **Tokyo Night** | dark / light | Cool blue-purple, modern. The default. |
| **Kanagawa** | wave (dark) / lotus (light) | Warm Japanese-inspired, muted sepia tones |

---

## 6. Feature Specifications

### 6.1 Import (`kyoku import <path>`)

Import **reads and catalogs** files into the database. It does NOT move, rename, or reorganize files. Moving files to your library structure is a separate deliberate action via `kyoku organize`.

The TUI import wizard pipeline:
```
Scan → Read Tags → Match (MusicBrainz) → Review → Duplicate Resolution → Write Tags → Update DB
```

The CLI import path is intentionally simpler today: scan/read tags and import as-is, with optional loose/collection handling. Note: files stay where they are. Their current paths are recorded in the database until `kyoku organize` moves/copies them.

#### 6.1.1 Scan Phase
- Recursively walk `<path>` using `walkdir`
- Filter by supported audio extensions: `.mp3`, `.flac`, `.ogg`, `.m4a`, `.wav`, `.wma`, `.ape`, `.opus`
- **Always audit `music_dir` alongside the inbox.** Every import scan also walks the configured `music_dir` looking for audio files that are not referenced by any DB row (`tracks.file_path`, `collection_tracks.collection_file_path`, `orphaned_files.file_path`). Untracked files are fed into the same import flow as inbox files — if a file lives under `music_dir` but the DB doesn't know about it, it needs to be imported or deleted, and the import wizard is the right place to decide.
- Group files into **album candidates** using heuristics:
  1. Files in the same directory = likely same album
  2. Existing album tag values (if present)
  3. Directory name parsing (`Artist - Album (Year)`)
- Skip files already in the database (by absolute path). When a duplicate is found, **warn** with the filename and skip it (do not silently ignore). This lets the user know what was skipped.

#### 6.1.2 Tag Reading Phase
- Read existing tags using `lofty`
- Extract: title, artist, album, album_artist, year, track_number, disc_number, genre, duration
- Read audio properties: bitrate, sample_rate, channels, file_size
- **If title tag is missing or empty**: derive title from filename by stripping the file extension and using the result as-is. Do not attempt to parse artist/title patterns from filenames or strip track numbers. Examples:
  - `deep house set recorded at bar.mp3` → title: `deep house set recorded at bar`
  - `01 Best Foot Forward.mp3` → title: `01 Best Foot Forward`
  - `少女綺想曲.flac` → title: `少女綺想曲`
- Store as `ImportCandidate` structs

#### 6.1.3 Fingerprint Phase (future / Milestone 9)
- Audio fingerprinting is not currently implemented.
- Planned flow: decode audio, generate Chromaprint, query AcoustID, and cache fingerprints locally for future matching/dedup.
- The schema already has `acoustid` / `chromaprint` columns reserved for this future work.

#### 6.1.4 Match Phase
- For each album candidate, query MusicBrainz for matching releases.
- Current matching strategies:
  1. **Manual MBID lookup**: user can paste a release MBID/URL in the TUI wizard.
  2. **Text search**: search MB by artist + album + track count, with follow-up full-release fetches for tracklists.
- Future matching strategy: **AcoustID match** once fingerprinting lands.
- Score matches by similarity (weighted combination of):
  - Artist name similarity (fuzzy string matching)
  - Album title similarity
  - Track count match
  - Total duration match (within tolerance)
  - Track title similarity (ordered comparison)
- Present top N candidates to user with similarity scores
- **Multiple releases of the same album** (e.g. US vs UK vs Japan editions): show all of them as separate candidates. Do not auto-select or filter by region. Let the user choose which edition they want.

#### 6.1.5 Duplicate Resolution

Before review, the wizard runs a two-pass duplicate check against the existing library and against the other groups being imported in the same batch. Duplicates are flagged *per track*, not per group, so a mixed album (two new tracks + one dup) is legal.

**Pass 1 — album-slot match (DB-only, fast).** For each incoming track that has an `album_id` (either because the batch is MB-matched or because tags alone already pick an existing album in the library), look for a library track with the same `(album_id, disc_number, track_number)` triple. Matches in this pass don't need a network round-trip — they are cheap enough to run on every batch.

**Pass 2 — MBID match (needs a fetched release).** For MB-matched groups whose release has actually been fetched (i.e. the tracklist is available), compare the incoming track's `recording_id` against `tracks.mbid` in the library and against other `AcceptMb` groups in the same batch. This pass is **skipped for any `(group_idx, track_idx)` pair already flagged by Pass 1** — album-slot is the stronger signal and there's no point asking the user twice. Release fetches are triggered lazily, piggy-backing on the user's MB-candidate selection: the moment a group transitions into `AcceptMb` (via keypress or auto-select) a background thread fetches its full release; the result lands via a channel and the preview refreshes. In practice the network call is rare because most album-level duplicates are caught by Pass 1.

The wizard presents each detected conflict with a short reason line (`Same slot on the same album.` vs `Same MusicBrainz recording.`) and three actions:

- **Keep New** — replace the library file. The existing track row is deleted; the old file is **not** removed from disk immediately — instead a row is inserted into `orphaned_files` with a snapshot of its identifying tags and `reason = "replaced by duplicate during import"`. The next `kyoku organize` run unlinks it. This two-phase cleanup gives the user a chance to back up or inspect the old file before it disappears.
- **Keep Other** — keep the library version; skip the incoming track.
- **Both** — import the new track anyway. The wizard does not try to auto-disambiguate paths; the organize step handles filename collisions with the usual numeric-suffix fallback.

Intra-batch conflicts (two incoming tracks collide with each other rather than with the library) use the same three actions; `Keep New` drops the *other* candidate instead of deleting a library row.

#### 6.1.6 Review Phase (TUI wizard)
- Display a **diff view** showing:
  - Current tag values (left column)
  - Proposed new values from MB (right column)
  - Changed fields highlighted
- User actions:
  - **Accept** — write proposed tags to file + add to DB
  - **Accept with edits** — modify individual fields before applying
  - **As-is** — add to DB with current tags, no tag modifications
  - **Skip** — don't import this album at all
  - **Manual search** — enter custom search query for MB

#### 6.1.7 Apply Phase
- Write tags to files using `lofty` (if `write_tags = true` and user accepted a match)
- Insert/update records in SQLite database (file_path points to CURRENT location)
- Files are NOT moved or renamed — they stay exactly where they are
- Log all tag modifications for potential undo

#### CLI Flags
```
kyoku import [path]
    (no path)               Import from all configured inbox_dirs
    --pretend / -p          Dry run: show what would happen without modifying anything
    --collection <name>     Add all imported tracks to a collection (creates it if needed)
    --loose                 Treat all files as individual tracks, don't try to group into albums
```

MusicBrainz review/matching currently lives in the TUI import wizard rather than the scriptable CLI path.

#### Handling Niche / Unmatched Content

MusicBrainz won't have entries for a lot of music: doujin releases, Bandcamp-only artists, field recordings, bootlegs, niche netlabels, etc. The import pipeline handles this gracefully:

1. **No match found** → offer "Import as-is" as the default action (not "skip")
2. **Low-confidence matches** → show them but don't pre-select; clearly label confidence
3. **CLI import** → imports as-is without MusicBrainz; useful for bulk-importing known-niche content
4. **`--loose` flag** → don't try to infer album structure; each file is independent
5. Unmatched tracks appear in all views, searches, and operations identically to matched ones
6. Tag status shows `unmatched` or `manual` — these are informational labels, not errors

#### 6.1.8 The import → organize flow

kyoku has two phases for getting your music into the library: **import** (cataloging) and **organize** (moving files into place). They're deliberately separate so you can review before anything moves on disk.

1. **Drop files into an inbox directory** — any path you've configured under `inbox_dirs`, or pass a path explicitly to `kyoku import <path>`.
2. **Run import.** kyoku scans the inbox, reads tags, optionally matches against MusicBrainz for clean metadata, checks for duplicates, and adds new tracks to the library database. Files stay where they are at this point — nothing is moved yet.
3. **Pick a flow per group during the wizard:**
   - **Album flow** (default) — kyoku detects albums automatically and keeps them grouped.
   - **Loose flow** (`--loose`) — each file is treated as standalone, no album grouping.
   - **Direct to collection** (`--collection "X"`) — every imported track joins the named collection (creating it if needed). Stack with `--loose` for "drop a folder of stray MP3s into a playlist".
4. **Run organize.** This is where files actually move from the inbox into your `music_dir`:
   - **Album tracks** → moved into the artist/album hierarchy via `path_template` (or `path_template_single_disc` for single-disc albums).
   - **Tracks in any collection** → an additional copy is placed in `Collections/<name>/...` via `collection_path_template`. One copy per collection. The per-collection `path_template` (if set) overrides the default.
   - **Loose tracks not in any collection** → moved into the special `_loose/` folder via `loose_path_template`.

   After organize, the inbox should be empty. Every file in your library lives somewhere under `music_dir`.

If a track exists in both an album and N collections, you end up with `1 + N` physical files: one in the album hierarchy, one per collection. Each is a real file that any file-browser music player can navigate independently.

When you delete a collection or remove a track from a collection in the TUI, you're offered an opt-in checkbox to also delete the corresponding files from disk. Files outside `music_dir` (e.g. user-relocated copies) are never touched. Tracks that would be left with no file home if you delete files are removed from the library entirely.

#### 6.1.9 Future import wizard enhancements
Nice-to-haves deferred from milestone 3 — the typed path input in the wizard is good enough for now but these make it noticeably better:

- **Path autocomplete / file picker** — a browsable directory picker in the import wizard (j/k to navigate, Enter to descend, Space to pick). Much nicer than typing absolute paths once users have a deeply-nested music staging area.
- **Recently-imported paths** — remember the last N paths entered in the wizard and show them as selectable suggestions above the text input. Persisted in a small state file or DB table. Saves re-typing a path when importing several albums from the same parent folder in one session.

### 6.2 Inbox Scan (`kyoku scan`)

Scans all configured `inbox_dirs` for audio files not yet in the database.

```bash
kyoku scan                   # Check all inbox dirs, show summary
```

In the TUI, the inbox status is always visible: `Inbox: 23 new files`. Pressing `i` on the inbox indicator starts the import wizard for those files.

The scan is lightweight — it only checks for new file paths not yet in the database. It does not read tags or fingerprint (that happens during import).

### 6.3 Library Search (TUI only)

**Design goal**: Search should feel like `rg` (ripgrep), not like SQL. A bare query just works.

Search is TUI-only and comes in two flavours — a **local filter** scoped to the current view, and a **global search** across the whole library.

#### Local Filter (`/`)
Press `/` to focus the search bar at the top of the current view. Typing immediately filters the view in place — no mode switching. `Esc` clears.

Scope depends on the current view:
- **Library browser**: filters albums by title/artist (FTS5-backed)
- **Album detail**: filters tracks inside the album by title/artist (in-memory)
- **Collection detail**: filters tracks inside the collection by title/artist (in-memory)
- **Collections**: filters the collection list by name (LIKE)

Queries are case-insensitive and multi-term (all terms must match somewhere). Unicode is handled natively — `東方` and `björk` work identically to ASCII.

#### Global Search (`g`)
Press `g` from any view to open a full-screen overlay that searches **everything** at once — albums, tracks and collections mixed together. Results are grouped by type and labelled `[album]`, `[track]`, `[coll]`. Selecting a result with Enter navigates to it directly (tracks jump to their containing album with the cursor positioned on the track).

Matching is simple fuzzy-ish: each whitespace-separated term must appear as a case-insensitive substring somewhere in the record's searchable fields. Unicode-aware. Backed by FTS5 on the DB side with a client-side fuzzy pass for ranking.

Use local filter when you know where you're looking and want to narrow; use global search when you just want to jump to something by name.

#### Filtered Search (future)
Planned for a later milestone: structured filters combinable with the freeform query — artist/album/title, genre, year, label, audio format, tag status, collection, loose tracks, import date range, bitrate range.

### 6.4 Collections (TUI only)

Collections are user-defined groupings that exist alongside (not instead of) the album hierarchy. They solve the "folder of random MP3s" problem. All collection management (create, browse, add/remove tracks, delete) is done through the TUI Collections view.

A collection can optionally have a custom path template. When `kyoku organize` runs, tracks in this collection stay grouped together in their own folder instead of being scattered across the artist hierarchy. This is the key feature for managing compilations, loose folders, doujin collections, DJ sets, and anything that doesn't fit the standard `Artist/Album/Track` structure.

If no per-collection template is set, kyoku uses the global `[library].collection_path_template`.

#### Collection order

A collection has its own order, independent of album track numbers. Once `collection_tracks.position` is set, that position is the single source of truth for display order and for `{position}` in collection path templates. `{track}` remains strictly the track-number tag from the audio metadata.

New collection memberships should always be assigned positions by appending to the collection while preserving the caller's intended order:

1. MusicBrainz-accepted imports use the matched release track order.
2. Album-like/cohesive metadata uses `(disc_number, track_number)` order.
3. Missing, duplicated, or scrambled track numbers fall back to scan/import/add order — never title order.

Legacy rows with `NULL` positions are interpreted with the same fallback policy at read/organize time: coherent metadata first, then `added_at`/`track_id` as a deterministic approximation of add order. Future manual reordering should update `collection_tracks.position` directly.

**Import directly to collection:**
```bash
# Quick and dirty — skip MB, no album grouping, just catalog into collection
kyoku import ~/Downloads/random-mp3s/ --loose --collection "Unsorted"

# TUI wizard version: match MB for good tags, then assign the group to a collection
kyoku  # launch TUI → Import → assign collection during review
```

**Tagging and organization are independent.** The `--collection` flag controls *where files end up* on disk (via the collection's path template during `kyoku organize`) and the order they appear in that collection. MusicBrainz matching in the TUI controls *what the tags say*. CLI imports are as-is today:

| Flow | Tags | Filesystem layout |
|------|------|-------------------|
| TUI import wizard, accept MB candidate | MB-matched | Global template (`Artist/Album/...`) |
| TUI import wizard, accept MB + assign collection | MB-matched | Collection template (`Collections/Touhou/...`) |
| `kyoku import ~/東方/ --collection "Touhou"` | As-is from files | Collection template (`Collections/Touhou/...`) |
| `kyoku import ~/東方/ --loose --collection "Touhou"` | As-is from files | Collection template (`Collections/Touhou/...`) |

### 6.5 Tag Editing (TUI only)

Tag editing is done through the TUI tag editor view. Select a track or album, open the editor, modify fields inline. All edits are reflected in both file tags and database. A preview diff is shown before applying changes.

### 6.6 File Organization (`kyoku organize`)

The deliberate "make my filesystem beautiful" command. This is where files actually move.

`kyoku organize` applies album, collection, and loose-track templates to move/copy files into the target structure under `music_dir`. It always shows a preview first and requires explicit confirmation.

```bash
kyoku organize                       # Preview what the entire library would look like
kyoku organize --apply               # Actually move files (requires confirmation)
kyoku organize --artist "Björk"     # Organize specific artist only
kyoku organize --album "Kid A"       # Organize specific album only
kyoku organize --path ~/Downloads/   # Organize only files currently under this path
kyoku organize --collection "Touhou" # Organize all tracks in a collection
kyoku organize --pretend             # Alias for preview (default behavior without --apply)
```

#### Template Resolution Order

When calculating target paths, kyoku applies templates by file role:

1. **Album copy** — `path_template_single_disc` for single-disc albums, otherwise `path_template`.
2. **Collection copy** — the collection's custom `path_template` if set, otherwise `[library].collection_path_template`. `{position}` is the collection order; `{track}` is still the track-number tag.
3. **Loose non-collection copy** — `[library].loose_path_template`.

#### Collection + Album: Dual-File Behavior

**A track can exist in both an album and a collection with a template.** When this happens, `kyoku organize` creates **two physical copies** of the file:

1. The **album copy** goes to the album hierarchy via the global template: `~/Music/DJ Shadow/Endtroducing..... (1996)/01 Best Foot Forward.mp3`
2. The **collection copy** goes to the collection folder via its template: `~/Music/Collections/Touhou/01 IOSYS - Marisa Stole the Precious Thing.mp3`

Both copies are tracked in the database. The collection copy is created via filesystem copy (not move). This means the user's collection folders are real, self-contained directories that a file-browser music player can browse independently from the album hierarchy.

For tracks that belong **only** to a collection (loose tracks with no album), there is only one copy — in the collection folder.

For tracks in collections **without** a custom template, kyoku uses `[library].collection_path_template`.

If a track belongs to multiple collections, it gets a copy in each collection folder. Each collection has its own independent `position` sequence.

The organize preview shows all copies that will be created:
```
~/Downloads/東方/IOSYS - Marisa Stole the Precious Thing.mp3
  → ~/Music/IOSYS/Marisa Stole the Precious Thing.mp3            (album)
  → ~/Music/Collections/Touhou/01 IOSYS - Marisa Stole the Precious Thing.mp3  (collection: Touhou)
```

#### Empty Directory Cleanup
After organize moves files out of their source directories, empty directories are **automatically deleted**. This applies to any directory that becomes empty as a result of the organize operation.

#### music_dir Creation
If `music_dir` does not exist when `kyoku organize --apply` is run, kyoku **asks for confirmation** before creating it. It does not create it silently or error out.

#### Workflow
1. Calculate target paths from current tags + resolved template (see priority above)
2. For tracks in collections with templates that also belong to albums, calculate both paths
3. Collect pending rows from `orphaned_files` — these are files whose track row has already been removed (typically by dup-replace during import) and are awaiting physical cleanup. They are included regardless of any `--artist` / `--album` filter because orphans don't belong to any current album.
4. Show full diff: `current path → target path(s)` for every file, labeling album vs collection copies, plus a dedicated "orphan files (will be deleted)" block
5. User reviews, can exclude individual files
6. On confirmation: move/copy files, update DB paths, unlink orphaned files and clear their tracking rows (file-not-found is treated as success so re-runs are idempotent), delete empty source directories
7. Files already in the correct location are skipped

#### TUI organize (`O` key)
Press `O` in the library browser to organize the entire library, or in album detail to organize just that album. The TUI shows a popup with two views:

- **Summary view** (default): groups of source → target directories with file counts, collection copy counts, orphan-track count (rows whose file is gone), orphan-file count (files whose row is gone — will be deleted), and "already in place" count. Press `d` to expand.
- **Detail view**: scrollable per-file listing showing each file's source path, target path, renamed filenames (highlighted), collection associations, orphaned tracks, and orphan files (labeled `Artist — Title · Album` from the tag snapshot, with the reason they were orphaned). Navigate with `j`/`k`, `PgUp`/`PgDn`, `Ctrl+U`/`Ctrl+D`. Press `d` to toggle back to summary.

`Enter` applies from either view. `Esc` goes back (detail → summary → close).

### 6.7 Library Relocation

There is no `kyoku relocate` command — there is nothing to relocate. Paths inside `music_dir` are stored relative to it (`tracks.file_path`, `albums.cover_art_path`, `collection_tracks.collection_file_path`, `orphaned_files.file_path`), so renaming the library directory only requires editing `[library].music_dir` in `config.toml`. The common case — the database lives inside `music_dir` so the rename moves it along with the music — needs no further action: open the TUI and everything resolves under the new prefix.

Paths outside `music_dir` (inbox files awaiting `kyoku organize`, user-relocated collection copies) stay absolute, so the rename rule doesn't touch them. The split is automatic — there's no flag to set.

If you're moving the library to a different drive, the same rule applies as long as the database file moves with it. If you keep the DB elsewhere, point `[library].data_dir` at the new DB location *and* `[library].music_dir` at the new music directory before launching kyoku.

Inspired by beets v2.10's switch to relative paths (see [beetbox/beets#133](https://github.com/beetbox/beets/issues/133)). Existing libraries migrate transparently the first time they're opened — the v7 schema migration rewrites any absolute path under the configured `music_dir` to its relative form. Paths outside are left untouched.

### 6.8 File Info (`kyoku info <path>`)

Display tags and audio properties for a single file. Works without a config file.

```
kyoku info /path/to/file.flac
```

Output includes: file path, format, title, artist, album, album artist, year, genre, track/disc number, duration, bitrate, sample rate, and tag status.

### 6.9 Setup (`kyoku setup`)

Interactive setup wizard. Walks you through configuring kyoku — music directory, database location, inbox directories, and theme.

```
kyoku setup
```

The wizard:
1. Explains what the config file is and where it lives
2. Asks for confirmation if a config already exists (won't silently overwrite)
3. Prompts for music directory (with current/default pre-filled)
4. Prompts for database directory (with default pre-filled)
5. Lets you add inbox directories one by one (Enter to skip/finish)
6. Lets you pick a theme
7. Writes a commented config file and confirms

The generated config includes all settings with comments. You can always edit it directly later.

### 6.10 Paths (`kyoku paths`)

Print the resolved paths kyoku is using. Useful for finding your config file or database.

```
kyoku paths
```

Output:
```
config:   ~/.config/kyoku/config.toml
database: ~/.local/share/kyoku/library.db
cache:    ~/.cache/kyoku
music:    ~/Music
inboxes:  ~/Downloads, ~/Music/Incoming
```

If the config file doesn't exist, a note is printed saying defaults are in use. If no inbox dirs are configured, the inboxes line shows `(none)`.

### 6.11 TUI Mode (`kyoku` or `kyoku tui`)

Interactive terminal UI for library management.

#### Views

**Library Browser (default)**
```
┌─ kyoku ─────────────── Inbox: 5 new ──── Library: 12,847 tracks ─┐
│ Search: █                                    [Albums ▸ Collections] │
├────────────────────────────────────────────────────────────────────┤
│ Artist              │ Album                │ Year │ Tracks │ Fmt  │
│─────────────────────│──────────────────────│──────│────────│──────│
│ Radiohead           │ OK Computer          │ 1997 │  12    │ FLAC │
│ Radiohead           │ Kid A                │ 2000 │  10    │ FLAC │
│ Björk               │ Homogenic            │ 1997 │  10    │ MP3  │
│ > Portishead        │ Dummy                │ 1994 │  11    │ FLAC │
│ 初音ミク             │ VOCALOID BEST        │ 2012 │   8    │ MP3  │
│ [loose]             │ 15 unalbumed tracks  │      │  15    │ mix  │
│                     │                      │      │        │      │
├────────────────────────────────────────────────────────────────────┤
│ Portishead - Dummy (1994) · FLAC · 11 tracks · 49:27              │
│ /Music/Portishead/Dummy (1994)/                                    │
└────────── j/k nav · Enter detail · i import · / search · c colls ┘
```

Note: `[loose]` is a virtual entry that groups all tracks without an album. It appears at the bottom of the list and can be expanded to browse individual loose tracks.

**Collections View** (triggered by `c` or Tab to switch)
```
┌─ kyoku ──────────────────────────────────── Collections ──────────┐
│ Search: █                                    [Albums ▸ Collections] │
├────────────────────────────────────────────────────────────────────┤
│ Collection               │ Tracks │ Description                   │
│──────────────────────────│────────│───────────────────────────────│
│ > Driving Music          │   34   │ Highway playlist              │
│   Unsorted 2024          │  127   │ From old hard drive           │
│   Doujin Music           │   89   │ Comiket acquisitions          │
│   Field Recordings       │   12   │ Japan trip 2023               │
│                          │        │                               │
├────────────────────────────────────────────────────────────────────┤
│ Driving Music · 34 tracks · 2h 13m                                │
└────────── j/k nav · Enter browse · n new · d delete · Tab albums ┘
```

**Album Detail View** (Enter on selected album)
```
┌─ Portishead — Dummy (1994) ──────────────────── Status: Verified ─┐
│                                                                    │
│  #  │ Title                    │ Duration │ Status    │ Bitrate    │
│  1  │ Mysterons                │   5:02   │ verified  │ 924 kbps   │
│  2  │ Sour Times               │   4:11   │ verified  │ 924 kbps   │
│  3  │ Strangers                │   3:56   │ verified  │ 924 kbps   │
│  ...                                                               │
│                                                                    │
│ MusicBrainz: release/76df3287-6cda-33eb-8e9a-044b5e15c37c         │
│ Label: Go! Beat                                                    │
├────────────────────────────────────────────────────────────────────┤
│ e edit tags · r re-match MB · m move/rename · a add to coll · Esc ┘
└────────────────────────────────────────────────────────────────────┘
```

**Import Wizard View** (triggered by `i` or `kyoku tui --import <path>`)
- Step-by-step guided import with the pipeline from 6.1
- Progress bar for scanning/fingerprinting
- Side-by-side diff for tag review
- Batch accept/skip controls
- **"Import as-is"** and **"Import loose"** always visible as options alongside match candidates

**Tag Editor View** (triggered by `e` on a track/album)
- Inline field editing
- Tab between fields
- Shows original ↔ modified diff
- Save/cancel/reset

#### Key Bindings
```
Global:
  q / Ctrl+C         Quit
  ? / F1             Help overlay
  /                  Focus local filter bar (filters current list in place)
  g                  Open global search (albums, tracks, collections)
  Esc                Back / cancel / close overlay / clear search
  Tab                Switch between Albums ↔ Collections views

Navigation (any list view):
  j / ↓              Move down
  k / ↑              Move up
  Ctrl+D             Half page down
  Ctrl+U             Half page up
  Ctrl+F / PgDn      Page down
  Ctrl+B / PgUp      Page up
  G                  Jump to bottom

Library Browser:
  Enter              Open album detail
  i                  Start import wizard
  O                  Organize entire library (preview → d for details → Enter apply)
  s                  Sort (cycle: artist, album, year, tracks)
  c                  Switch to collections view

Album Detail:
  /                  Filter tracks in this album
  e                  Edit tags (tag editor)
  R                  Rename album
  O                  Organize this album (preview → Enter apply)
  a                  Add track(s) to a collection
  o                  Open file location in system file manager
  Esc                Back to library

Collections:
  Enter              Browse collection
  n                  Create new collection
  d                  Delete collection (confirms)
  Tab                Switch to albums

Collection Detail:
  /                  Filter tracks in this collection
  e                  Edit tags
  x                  Remove track from collection (confirms)
  Esc                Back to collections

Global Search (g):
  Type               Query across albums, tracks and collections
  j / k              Navigate results
  Enter              Open selected result (albums/collections directly,
                     tracks jump to their album with cursor positioned)
  Esc                Close

Import Wizard:
  A                  Import as-is (no MB match, keep current tags)
  S                  Skip group
  L                  Import loose (no album grouping)
  a                  Accept match (milestone 4)
  n / p              Next / previous group
  Enter              Confirm and import
  e                  Edit before applying (milestone 4)

Tag Editor:
  Enter              Edit selected field
  Tab                Next field
  Ctrl+S             Save changes to DB
  Esc                Cancel edit / back to previous view
```

---

## 7. Path Template Engine

The path template engine replaces `{variable}` placeholders with sanitized tag values.

### Config templates

There are four templates in `[library]`, each used in a different situation by `kyoku organize`:

| Setting | Used for | Default |
|---------|----------|---------|
| `path_template` | Multi-disc album tracks (album hierarchy) | `{album_artist}/{album} ({year})/{disc:0}-{track:02} {title}.{ext}` |
| `path_template_single_disc` | Single-disc album tracks (album hierarchy) | `{album_artist}/{album} ({year})/{track:02} {title}.{ext}` |
| `collection_path_template` | Collection copies (one per collection a track is in) | `Collections/{collection}/{position:02} {album_artist} - {title}.{ext}` |
| `loose_path_template` | Loose tracks with no album and no collection | `_loose/{artist} - {title}.{ext}` |

A collection can also have its own per-collection `path_template` (in the `collections` table) that overrides `collection_path_template` for that collection only. Collection templates should use `{position}` for collection order; `{track}` always means the file's own track-number tag.

### Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `{artist}` | Track artist | `Radiohead` |
| `{album_artist}` | Album artist (falls back to track artist) | `Radiohead` |
| `{album}` | Album title | `OK Computer` |
| `{year}` | Release year | `1997` |
| `{title}` | Track title | `Paranoid Android` |
| `{track}` | Track-number tag from the file / album metadata | `2` |
| `{disc}` | Disc number | `1` |
| `{position}` | Collection position (collection templates only) | `7` |
| `{genre}` | Primary genre | `Alternative Rock` |
| `{ext}` | File extension (lowercase, no dot) | `flac` |
| `{label}` | Record label | `Parlophone` |
| `{collection}` | Collection name (for collection templates) | `Touhou` |

### Format Specifiers

- `{track:02}` → `02` (zero-padded track-number tag)
- `{position:02}` → `07` (zero-padded collection position)
- `{disc:0}` → `1` (no padding, just the number, omitted for single-disc)
- `{year:4}` → `1997`

### Sanitization Rules

1. Replace `/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|` with `_`
2. Trim leading/trailing whitespace and dots
3. Collapse multiple consecutive underscores
4. Truncate to OS filename limit (255 bytes) with hash suffix if needed
5. Handle Unicode correctly (no ASCII folding)

---

## 8. Unicode & CJK Handling

This is a first-class concern, not an afterthought. Every layer of the application must handle non-Latin text correctly.

### Tag I/O
- `lofty` handles multi-byte encodings correctly for all formats. Always read/write as UTF-8 internally.
- When writing ID3v2 tags, use UTF-8 encoding (ID3v2.4) or UTF-16 (ID3v2.3) — never Latin-1 for CJK content.
- Preserve original encoding when possible during read-modify-write cycles.
- Handle mixed-script content: an album might have `artist = "初音ミク"` and `album = "VOCALOID BEST 2012"`.

### Filesystem
- Use `OsString`/`OsStr` for filesystem operations, convert to/from UTF-8 only at display boundaries.
- macOS uses NFD normalization for filenames; Linux typically uses NFC. Preserve original path bytes/strings when operating; add explicit NFC/NFD normalization only if comparison bugs surface.
- Japanese filenames may contain fullwidth characters (Ａ vs A); preserve them in tags and filenames unless a future normalization feature is explicitly added.
- **WSL note**: When music files live on a Windows NTFS mount (`/mnt/c/...`), filenames are case-insensitive and certain characters (`:`, `*`, `?`, etc.) are forbidden. The sanitization rules in the path template engine (Section 7) already handle this (including Windows reserved device names, dodged on all platforms). The same rules apply when running natively on Windows; the path test battery should treat NTFS semantics (case-insensitivity, reserved names, no trailing dots/spaces) as a first-class case regardless of host OS.
- Test filenames with characters from: Japanese (hiragana/katakana/kanji), Chinese (simplified/traditional), Korean (hangul), Polish (ą, ć, ę, ł, ń, ó, ś, ź, ż), Nordic (å, ä, ö, ø), and mixed scripts.

### Search
- FTS5 `unicode61` tokenizer handles CJK by tokenizing each character individually, which works for substring-style search. `remove_diacritics 2` maps accented characters (ą→a) while keeping the original indexed too.
- Search for `初音` should match `初音ミク`. Search for `bjork` should match `Björk`.
- TUI/global search helpers must remain Unicode-aware. Do not assume ASCII or byte-level matching.

### TUI Display
- Use `unicode-width` crate to calculate display widths. CJK characters are typically 2 columns wide. Table column alignment must account for this.
- Never truncate in the middle of a multi-byte character or grapheme cluster.
- Consider `unicode-segmentation` for grapheme-cluster-aware truncation if simple char-boundary truncation proves insufficient.
- Test TUI rendering with mixed Latin + CJK content in the same table row.

### Script preference for MB-derived names
A single MB entity can have both a native-script primary name and one or more Latin-script aliases. Users differ on which they want on disk — JP fans typically want `ヨルシカ` / `花冷え。`; Latin-library users want `Yorushika` / `HANABIE.`. The `[musicbrainz] name_script` setting (`native` | `latin`) drives a post-fetch alias resolver that rewrites `MbRelease.artist` and `MbRelease.title` before they enter the DB/tag pipeline. Scope is deliberately limited to artist + album title — track titles ride through unchanged because MB's alias coverage at the recording level is sparse. When no alias matches the preference, the canonical credit name is kept (no synthesised romanisation). Per-artist alias responses are cached on the `MbClient` for the lifetime of an import session so a multi-release import of the same artist pays the `/artist/{mbid}?inc=aliases` cost only once.

### Sorting
- Use ICU-aware collation for sorting when possible. At minimum, case-insensitive sorting that handles Unicode correctly.
- Consider the `icu_collator` crate or simpler approaches like lowercasing with `str::to_lowercase()` (which handles Unicode case folding correctly in Rust).
- Sort names: respect sort-order tag fields from files (e.g. `ARTISTSORT = "ハツネミク"` for kana-based sorting of Japanese content).

### Unicode dependencies / candidates

| Crate | Status | Purpose |
|-------|--------|---------|
| `unicode-width` | current | Display width calculation (CJK = 2 columns) |
| `unicode-normalization` | future candidate | NFC/NFD normalization for path comparison if needed |
| `unicode-segmentation` | future candidate | Grapheme-cluster-aware truncation/editing if current char-boundary handling proves insufficient |

---

## 9. Error Handling Strategy

### Principles
- Never panic on user data (malformed tags, missing fields, bad filenames)
- Collect errors per-file during batch operations, report summary at end
- Network errors should be retryable with backoff
- Tag write failures must not leave files in a half-written state (write to temp, then atomic rename)

---

## 10. Implementation Roadmap

### Milestone 1: Foundation (Core + basic read-only operations)
**Goal**: You can scan files and read/display their tags.

- [x] Project scaffolding (Cargo.toml, module structure, error types)
- [x] Configuration loading (TOML parsing, XDG paths, defaults, inbox_dirs)
- [x] Database schema + migrations (rusqlite, `migrations/001_initial.sql`)
- [x] Tag reader abstraction over `lofty` (read all supported formats)
- [x] `kyoku info <path>` — display file metadata and tags
- [x] `kyoku setup` — interactive config wizard
- [x] `kyoku paths` — show resolved config/data/cache paths
- [x] Basic test fixtures (short silence clips with known tags, including CJK-tagged files)

### Milestone 2: Library Database + Import
**Goal**: You can import files into a database and query them.

- [x] Import files into SQLite (scan → read tags → insert, files stay in place)
- [x] `kyoku import <path>` (local-only, no MB matching yet)
- [x] `kyoku import --loose` mode
- [x] `kyoku scan` — inbox directory scanner (checks configured inbox_dirs)
- [x] Search with FTS5 (freeform + filters) — TUI only
- [x] Collections (create, add, list, show, remove, delete) — TUI only
- [x] `kyoku import --collection` integration

### Milestone 3: TUI (primary interface)
**Goal**: Full interactive terminal UI — this is what users will spend most time in.

- [x] TUI app skeleton (ratatui + crossterm, event loop, view routing, panic hook)
- [x] Library browser view (sortable table, inline search bar, loose tracks section)
- [x] Collections view (Tab to switch, create/browse/delete with confirmation)
- [x] Collection detail view (track listing, remove track with confirmation)
- [x] Album detail view (track listing, album rename with `R`)
- [x] Import wizard (guided pipeline with progress, as-is/skip/loose options; MB matching stubbed for milestone 4)
- [x] Tag editor view (inline field editing; DB-only for now, file tag writing deferred)
- [x] Inbox indicator (shows count of unimported files from inbox_dirs)
- [x] Key bindings + help overlay
- [x] Theming support (4 built-in themes in `themes.rs`, selected from config, semantic color slots)
- [x] Two-mode search: local filter (`/`) per-view + global search (`g`) across albums/tracks/collections with fuzzy-ish matching
- [x] FTS5 triggers (migration `002_fts_triggers.sql`) for auto-synced full-text index

### Milestone 4: MusicBrainz Integration
**Goal**: You can match albums against MusicBrainz and auto-tag.

- [x] MusicBrainz text search (artist + album) — `src/external/musicbrainz.rs`, reqwest blocking + JSON
- [x] Match scoring algorithm (Jaro-Winkler fuzzy matching, weighted multi-field) — `src/external/matching.rs`
- [x] Import wizard MB matching integration (TUI) — scan → match → review with candidates → import
- [x] Tag writing (write matched data back to files via lofty) — `tagger::write_tags()`
- [x] Config cleanup: hardcoded user agent from CARGO_PKG_VERSION, removed from config
- [x] Configurable script preference for MB-derived artist/album names (`[musicbrainz] name_script = "native" | "latin"`) — fetches MB aliases and picks the preferred variant at commit time, with per-artist alias caching to stay within MB's rate limit.
- [ ] `--pretend` mode for all mutating commands

### Milestone 5: File Organization + Library Management
**Goal**: Beautiful filesystem output, with full user control.

- [x] Path template engine (`src/core/template.rs`) — variables, format specifiers, sanitization, CJK-safe
- [x] `kyoku organize` — preview + apply file reorganization (`src/core/organizer.rs`)
- [x] `kyoku organize --apply` with filters: `--artist`, `--album`, `--path`, `--collection`
- [x] Collection dual-copy support (album copy + collection copy with custom template)
- [x] Relative-path storage — `tracks.file_path` / cover / collection-copy / orphan paths are stored relative to `music_dir` (schema v7), so renaming the library directory only needs a config edit (`src/core/paths.rs`, `migrations/007_*` in `src/db/schema.rs`). Inspired by beets v2.10.
- [x] Clean up empty directories after moves (recursive parent cleanup)
- [x] `kyoku organize` TUI integration (`O` key — library: organize all, album detail: organize album; summary/detail views with scrollable per-file listing)
- [x] Filesystem output honours `[musicbrainz] name_script` — artist dirs and album-title segments follow the Latin/native preference resolved at MB-fetch time (see Milestone 4)
- [x] Collection order polish — populate `collection_tracks.position` on every add/import path, use it as collection order source of truth, add `{position}` for collection templates, and fall back sensibly for legacy NULL positions

### Milestone 6: Deletion, Cover Art, Full Tag Editing
**Goal**: Round out the core library-management surface.

- [x] Multi-select (`Space` toggles rows) in library and album-detail views
- [x] Batch delete of tracks / albums / collections with opt-in file deletion (keep-files is the confirm-popup default)
- [x] Album cover schema (`albums.cover_art_path`) + adopt sibling `cover.jpg` / `folder.jpg` on import, moved alongside audio on organize
- [x] TUI album cover preview (kitty/iterm2/sixel; preview slot dropped on multiplexers and halfblocks-only terminals, with cover filename surfaced in the info panel instead)
- [x] Opt-in cover fetch from Cover Art Archive (`coverartarchive.org/release/{mbid}`) with configurable size (`[musicbrainz] cover_art_size`) and overwrite confirmation
- [x] Full tag view on track edit — every standard `lofty::ItemKey` frame, grouped by kind (Standard / MusicBrainz / ReplayGain)
- [x] Inline tag editor (edit existing frames, clear frame to delete, multi-value preserved via `|` separator) respecting `[tagging] write_tags`; atomic file write via copy-tmp-rename

### Milestone 7: Import Hygiene — Duplicate Resolution & Orphan Flow
**Goal**: Catch duplicates at import time before they pollute the library, and wire a safe two-phase cleanup for the files they replace.

- [x] Two-pass duplicate detection in the import wizard:
  - Pass 1 — album-slot: match incoming `(album_id, disc, track)` triples against the library and against other `AcceptMb` groups in the same batch (DB-only, no network)
  - Pass 2 — MBID: compare `recording_id` against `tracks.mbid` and intra-batch peers, skipping `(group, track)` pairs Pass 1 already flagged
- [x] Lazy full-release fetch — when a group transitions into `AcceptMb` (user keypress or auto-select), a background thread calls `fetch_release` so the MBID pass has a tracklist to compare against. Dedup-protected so each group is fetched at most once.
- [x] Per-track Keep New / Keep Other / Both resolution — a conflict list is surfaced in the wizard with a short reason (`Same slot on the same album.` / `Same MusicBrainz recording.`)
- [x] `orphaned_files` table: `Keep New` deletes the library track row immediately but leaves the file on disk and records a tracking row with a snapshot of identifying tags + the reason for orphaning
- [x] `kyoku organize` integration: the plan includes pending orphan-file cleanups regardless of filter; apply unlinks each file and clears its tracking row (file-not-found is treated as success — repeated runs are idempotent). Emptied parent dirs are swept the same way as regular moves.
- [x] Organize preview surfaces orphan files in both summary (count + "will be deleted" block) and detail view (label + path + reason)
- [x] Notices in library, collection, and CLI organize paths report `N orphan files deleted` alongside moves/copies
- [x] Import scan audits `music_dir` for audio files not referenced by any DB row (`tracks.file_path`, `collection_tracks.collection_file_path`, `orphaned_files.file_path`) and feeds them into the same wizard as inbox files — the user either imports them or marks them for deletion

### Milestone 8: Device Sync
**Goal**: One-shot sync your library to external devices — MP3 players, SD cards, USB drives — entirely from the TUI. No saved configuration; each sync is an interactive wizard. Device-first workflow: pick the device, not a directory.

**TUI Sync Wizard flow:**
1. **Pick device** — detect and list removable/external block devices (via `lsblk` filtered to USB/SD/removable). Show device name, label, size, filesystem type.
2. **Confirm mount point** — show where the selected device is currently mounted. If not mounted, offer to mount it. User selects/confirms the mount point from a list (devices may have multiple partitions).
3. **Pick source** — entire library, a specific collection, or a search query result.
4. **Options** — toggle flags: delete files missing from source, run fatsort after sync, path template override.
5. **Preview** — dry-run diff: files to add, files to remove (if delete enabled), files unchanged. Show counts and total size.
6. **Sync** — copy/delete files with live progress bar. If fatsort enabled: unmount device → run `fatsort <device>` → remount. Show summary on completion.

**Tasks:**
- [ ] Device detection (`core/sync.rs`): list removable block devices via `lsblk --json`, filter to USB/SD/removable, parse mount points and filesystem types
- [ ] Sync engine: resolve source track list, compute file diff against mount point, copy/delete files
- [ ] fatsort integration: after sync, unmount device → `fatsort [-n] <device>` → remount. Requires the block device path (known from step 1), not the mount point.
- [ ] TUI sync wizard (`tui/views/sync.rs`): step-through flow — pick device → confirm mount → pick source → set flags → preview diff → confirm → sync with progress
- [ ] Source selection: entire library, a collection, or a search query
- [ ] Mount point confirmation/selection (device may have multiple partitions)
- [ ] Dry-run preview: show files to add/remove/skip with counts and total size
- [ ] Live progress bar during file copy
- [ ] Optional delete mode: remove files on destination absent from source, with explicit confirmation showing count
- [ ] Optional per-sync path template override
- [ ] fatsort post-sync cycle: unmount → fatsort → remount, with clear TUI status for each step

### Milestone 9: Audio Fingerprinting
**Goal**: Identify music from its audio content when tags are missing or wrong.

- [ ] Choose fingerprinting stack (likely audio decode → Chromaprint-compatible fingerprint)
- [ ] AcoustID lookup integration — query acoustid.org with fingerprint + duration, get MB recording IDs
- [ ] Import wizard integration: optional fingerprint-based matching alongside text search
- [ ] AcoustID API key in config (`[acoustid] api_key`)
- [ ] CLI/config toggle to skip fingerprinting when this feature exists

### Future (v2): AI Integration
**Goal**: Optional AI-powered features via OpenAI-compatible API.

- [ ] `ai.rs` client (configurable endpoint — supports local Ollama/vLLM or cloud)
- [ ] Genre classification from audio analysis
- [ ] Smart duplicate detection (semantic, not just fingerprint)
- [ ] Natural language library queries ("find all 90s albums I imported last month")
- [ ] Recommendation engine ("similar to albums I have by Portishead")

---

## 11. Testing Strategy

### Unit Tests
- Path template engine (edge cases: Unicode, empty fields, long filenames)
- Match scoring algorithm (known similarity inputs → expected scores)
- Database queries (using in-memory SQLite)
- Config parsing (valid, invalid, missing fields, defaults)
- Tag reading abstraction (mock lofty responses)

### Integration Tests
- Full import pipeline with fixture audio files
- MusicBrainz matching with recorded/mocked API responses
- File rename/move operations (temp directory, verify structure)
- Database migrations (fresh + upgrade scenarios)

### Test Fixtures
- Create short (1-second) silence audio files in each supported format with known tag values
- Store in `tests/fixtures/sample_library/`
- Include edge cases: missing tags, Unicode tags (Japanese, Polish characters), multi-disc albums

---

## 12. Explicit Behavior Rules

These are unambiguous rules for edge cases. Do not deviate from them.

1. **Duplicate import (same file path already in DB)**: Print a warning with the filename, then skip. Do not silently ignore. Do not re-import or update.
2. **Missing title tag**: Derive title from filename by stripping the file extension only. Use the result as-is. Do NOT attempt to parse `Artist - Title` patterns, strip track numbers, or clean up the filename in any way. Example: `01 Best Foot Forward.mp3` → title is `01 Best Foot Forward`.
3. **MB returns multiple releases** (US vs UK vs Japan edition of same album): Show ALL of them as separate match candidates with their country/label/year. Do not auto-select or filter by region. Let the user choose.
4. **Track in album AND collection with template**: During `kyoku organize`, create TWO physical copies — one in the album hierarchy (global template), one in the collection folder (collection template). The collection copy is a filesystem copy, not a move. Both paths are tracked in the DB (`tracks.file_path` for the album copy, `collection_tracks.collection_file_path` for the collection copy).
5. **Track in collection only (no album)**: Only one copy, in the collection folder.
6. **Collection without template**: Purely virtual grouping. No extra copies during organize. Track uses global template.
7. **Track in multiple collections with templates**: Gets a copy in EACH collection folder, plus the album copy if applicable.
8. **Empty directories after organize**: Delete automatically. No confirmation needed.
9. **`music_dir` doesn't exist**: When `kyoku organize --apply` is run, ask for confirmation before creating it. Do not create silently. Do not error out.
10. **Cover art detection**: On import, check for common folder art filenames (`cover.jpg`, `cover.png`, `folder.jpg`, `folder.png`, `front.jpg`, `front.png`, `artwork.jpg`) in the same directory as the audio files. Store the path in `albums.cover_art_path`. Do not extract embedded art to disk — read it from files on demand when needed.
11. **Supported audio formats**: Support every format that `lofty` can read/write. At minimum: MP3, FLAC, M4A/AAC, OGG, Opus, WAV, WMA, APE, AIFF. Do not artificially restrict to a subset.
12. **Library scale**: The target is 20,000–100,000 tracks. All DB queries must use indexes. TUI must use virtual scrolling / lazy loading for large result sets. Search must return results within 100ms at 100k tracks.
13. **Sync — no removable devices detected**: Show a clear message ("No removable devices found — plug in your device and try again"). Do not fall back to a manual path input.
14. **Sync — device not mounted**: If the selected device/partition is not mounted, offer to mount it. If mounting fails (permissions, no filesystem), show the error. Do not proceed with sync to an unmounted device.
15. **Sync — fatsort not installed**: If the user enables fatsort but `fatsort` is not found in PATH, show a warning *before* sync starts (at the options step). Let them proceed without fatsort or cancel.
16. **Sync — fatsort requires unmount**: After file copy, the wizard unmounts the device, runs `fatsort <device>`, and remounts. Show each step's status in the TUI. If unmount fails (device busy), warn and skip fatsort — do not force unmount.
17. **Sync with delete**: Always show a confirmation with the count and list of files that will be deleted before proceeding. Never auto-delete.
18. **Config file required**: All commands except `setup`, `paths`, and `info` require a config file. If missing, print an error directing the user to run `kyoku setup`. Running bare `kyoku` without a config shows a welcome message suggesting `kyoku setup`.

