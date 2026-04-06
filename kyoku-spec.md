# kyoku (曲) — Music Library Manager

> A TUI-first music library manager written in Rust.
> Curates a beautiful, browsable filesystem that any music player can navigate — because the file tree IS the product.

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
- Not a native Windows GUI app (runs on Windows via WSL)

### Platform Support
- **macOS** — primary development target
- **Linux** — full support (native)
- **Windows** — supported via WSL (Windows Subsystem for Linux)

The pure Rust stack (ratatui/crossterm, rusqlite bundled, symphonia, rusty-chromaprint) has zero system dependencies and compiles on all three platforms without conditional code. Crossterm handles terminal abstraction across platforms. Filesystem paths use `std::path::Path` throughout (never hardcoded separators).

### Problems with Beets (Why This Exists)

These are the specific frustrations this project aims to solve:

1. **Search is clunky and complex.** Beets' query language (`beet ls artist::^Radio album:OK`) is powerful but hostile to casual use. kyoku should feel like `ripgrep` for music: a single freeform query works 90% of the time, structured filters available when needed but never required.

2. **Custom compilations and loose collections are a nightmare.** Beets enforces a strict Artist → Album → Track hierarchy. Real music libraries have mixtapes, DJ sets, random MP3s in a folder, personal compilations, soundtracks with 40 different artists. kyoku treats "a folder of loosely related files" as a first-class concept, not a pathological edge case.

3. **Niche content with poor metadata service coverage.** Doujin music, indie netlabels, Bandcamp-only releases, field recordings — MusicBrainz simply doesn't have entries for much of this. Beets pushes hard toward "everything must match." kyoku treats unmatched content as normal citizens, not second-class items needing to be fixed.

4. **Poor Unicode / CJK handling.** Japanese, Chinese, Korean characters in tags, filenames, and search. Beets has recurring issues with encoding, display, sorting, and filesystem operations for non-Latin content. kyoku must handle CJK as a core concern, not an afterthought.

5. **Library relocation is painful.** Moving a library to a new drive or path in beets requires manual SQLite queries. kyoku has a built-in `kyoku relocate` command that rebases all paths in one operation.

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
│                               │  - AcoustID   │           │
│                               │  - (future AI)│           │
│                               └──────────────┘           │
└─────────────────────────────────────────────────────────┘
```

### Crate / Module Layout

```
kyoku/
├── .mise.toml           # Rust toolchain version (managed by mise)
├── justfile             # Task runner (just) — all dev commands
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point, CLI dispatch
│   ├── cli/
│   │   ├── mod.rs           # clap App definition
│   │   ├── import.rs        # `kyoku import` command
│   │   └── setup.rs         # `kyoku setup` interactive wizard
│   ├── tui/
│   │   ├── mod.rs           # TUI app struct, main loop
│   │   ├── app.rs           # App state, event handling
│   │   ├── views/
│   │   │   ├── library.rs   # Library browser view
│   │   │   ├── import.rs    # Import wizard view
│   │   │   ├── detail.rs    # Album/track detail view
│   │   │   ├── search.rs    # Search/filter view
│   │   │   ├── edit.rs      # Tag editor view
│   │   │   └── sync.rs     # Sync wizard view (device sync)
│   │   ├── widgets/
│   │   │   ├── table.rs     # Sortable, filterable table
│   │   │   ├── diff.rs      # Tag diff display
│   │   │   ├── progress.rs  # Import progress bar
│   │   │   └── input.rs     # Text input widget
│   │   └── keybindings.rs   # Key mapping configuration
│   │   └── themes.rs       # 4 built-in color themes
│   ├── core/
│   │   ├── mod.rs
│   │   ├── library.rs       # Library operations (add, remove, query)
│   │   ├── importer.rs      # Import pipeline (scan → match → tag → move)
│   │   ├── tagger.rs        # Tag read/write abstraction over lofty
│   │   ├── matcher.rs       # MusicBrainz matching logic
│   │   ├── renamer.rs       # Path template engine
│   │   ├── fingerprint.rs   # AcoustID/Chromaprint integration
│   │   └── sync.rs          # Device sync logic (file diff, copy/delete, fatsort)
│   ├── db/
│   │   ├── mod.rs
│   │   ├── schema.rs        # Table definitions, migrations
│   │   ├── models.rs        # Rust structs ↔ DB rows
│   │   └── queries.rs       # Prepared statements, search
│   ├── config/
│   │   ├── mod.rs
│   │   ├── settings.rs      # Configuration struct + defaults
│   │   └── paths.rs         # XDG path resolution
│   ├── external/
│   │   ├── mod.rs
│   │   ├── musicbrainz.rs   # MB API wrapper (uses musicbrainz_rs)
│   │   └── acoustid.rs      # AcoustID lookup
│   └── error.rs             # Unified error types (thiserror)
├── migrations/
│   └── 001_initial.sql
├── config/
│   └── default.toml         # Default configuration
└── tests/
    ├── tag_reader_test.rs  # Integration tests for tag reading
    └── fixtures/
        ├── create_fixtures.rs  # Helper to generate test audio files
        └── sample_library/     # Test audio files (short silence clips with tags)
```

---

## 3. Technology Stack

### Core Dependencies

| Crate | Purpose | Version / Notes |
|-------|---------|-----------------|
| `ratatui` | TUI framework + terminal backend | Latest stable; uses built-in `crossterm` feature (default) for cross-platform terminal I/O |
| `clap` | CLI argument parsing | v4, derive API |
| `lofty` | Audio metadata read/write | Multi-format: MP3/FLAC/OGG/M4A/WAV/WMA/APE |
| `musicbrainz_rs` | MusicBrainz API client | v0.9+, async with built-in rate limiter |
| `rusty-chromaprint` | Audio fingerprinting | Pure Rust, no C deps |
| `rusqlite` | SQLite database | With `bundled` feature for zero system deps |
| `tokio` | Async runtime | For network I/O (MB, AcoustID) |
| `serde` / `toml` | Configuration parsing | TOML config files |
| `thiserror` / `anyhow` | Error handling | `thiserror` for library errors, `anyhow` in main/CLI |
| `walkdir` | Recursive directory scanning | Fast filesystem traversal |
| `dirs` | XDG directory resolution | Config/data/cache paths |
| `inquire` | Interactive CLI prompts | Setup wizard |
| `fuzzy-matcher` | Fuzzy search | For library search/filtering |
| `symphonia` | Audio decoding | Decode audio for fingerprinting (pure Rust) |

### Why These Choices

- **lofty over id3**: `lofty` handles all major audio formats with a unified API. `id3` is MP3-only. A music library manager must handle FLAC, OGG, M4A, etc.
- **rusqlite over sqlx**: Simpler setup, no need for compile-time query checking for this use case, `bundled` feature means zero external deps.
- **symphonia for decoding**: Pure Rust audio decoder needed to feed PCM samples to `rusty-chromaprint`. No FFmpeg dependency.
- **ratatui**: The dominant, actively maintained Rust TUI framework. Its built-in crossterm backend provides cross-platform terminal support (Windows, macOS, Linux) — no separate crossterm dependency needed.

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
    mbid            TEXT UNIQUE,        -- MusicBrainz release group ID
    release_mbid    TEXT,               -- MusicBrainz release ID (specific edition)
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
-- collections AND also belong to an album. Collections don't affect tags or filenames.
CREATE TABLE IF NOT EXISTS collections (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    description TEXT,
    -- Path template override (optional). If set, tracks in this collection
    -- use this template instead of the global one when organized.
    -- e.g. "Collections/{collection}/{track:02} {artist} - {title}.{ext}"
    path_template TEXT,
    created_at  TEXT DEFAULT (datetime('now')),
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS collection_tracks (
    collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    track_id      INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    position      INTEGER,             -- Optional ordering within collection
    -- When the collection has a path_template, organize creates a physical copy
    -- of the file in the collection folder. This field tracks that copy's path.
    -- NULL if collection has no template or file hasn't been organized yet.
    collection_file_path TEXT,
    added_at      TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (collection_id, track_id)
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

### Rust Models

```rust
// In db/models.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Option<i64>,
    pub album_id: Option<i64>,       // None = loose track (not part of any album)
    pub title: String,
    pub artist: Option<String>,       // Track artist (may differ from album artist)
    pub track_number: Option<u32>,
    pub disc_number: u32,
    pub duration_ms: Option<u64>,
    pub mbid: Option<String>,        // Optional — niche content won't have one
    pub file_path: PathBuf,
    pub file_format: AudioFormat,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub tag_status: TagStatus,
    pub source_dir: Option<PathBuf>, // Preserved original directory context
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TagStatus {
    Unmatched,   // Imported but not matched to MB — this is fine, not an error
    Matched,     // Auto-matched via MB
    Verified,    // User confirmed the match
    Manual,      // User manually edited tags
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlbumType {
    Album, Compilation, Single, Ep, Soundtrack, Live, Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AudioFormat {
    Mp3, Flac, Ogg, M4a, Wav, Wma, Ape, Opus, Aiff, Unknown(String),
}

/// A collection is a user-defined grouping orthogonal to albums.
/// Use cases: "random MP3s from 2005", "DJ set recordings", "to-sort inbox",
/// "my running playlist", "doujin music unsorted".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    pub path_template: Option<String>,  // Override global path template
    pub track_count: u32,               // Computed, not stored
}
```

---

## 5. Configuration

### Location
Following XDG Base Directory spec (via `dirs` crate, which resolves platform-appropriate paths):

| | Linux / WSL | macOS |
|---|---|---|
| Config | `~/.config/kyoku/config.toml` | `~/Library/Application Support/kyoku/config.toml` |
| Database | `~/.local/share/kyoku/library.db` | `~/Library/Application Support/kyoku/library.db` |
| Cache | `~/.cache/kyoku/` | `~/Library/Caches/kyoku/` |

All paths are overridable via `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_CACHE_HOME` environment variables on any platform. The `dirs` crate handles this automatically.

### config.toml

```toml
[library]
# Root directory for managed music files
music_dir = "~/Music"

# Inbox directories — kyoku scans these for new/unimported files.
# Shown in the TUI as "Inbox" with a count of pending items.
# `kyoku scan` also checks these paths.
inbox_dirs = [
    "~/Downloads",
    "~/Music/Incoming",
]

# Path template for organizing files (used by `kyoku organize`)
# Available variables: {artist}, {album_artist}, {album}, {year}, {track}, 
#                      {title}, {disc}, {genre}, {ext}, {artist_sort}
# Use {track:02} for zero-padded track numbers
path_template = "{album_artist}/{album} ({year})/{disc:0}-{track:02} {title}.{ext}"

# Template for single-disc albums (disc_total == 1)
path_template_single_disc = "{album_artist}/{album} ({year})/{track:02} {title}.{ext}"

[import]
# Import behavior: import READS and CATALOGS files into the database.
# It does NOT move or rename files by default. Use `kyoku organize` for that.
# This setting controls what happens when you explicitly organize:
# Options: "move" (move to music_dir), "copy" (copy, keep originals)
organize_operation = "move"

# Automatically accept matches with similarity above this threshold (0.0 - 1.0)
auto_match_threshold = 0.95

# Use AcoustID fingerprinting for matching (requires network)
use_fingerprint = true

# Number of MusicBrainz match candidates to display
match_candidates = 5

# Skip files already in the library (by path or fingerprint)
skip_duplicates = true

[tagging]
# Write tags back to files (if false, only updates DB)
write_tags = true

[musicbrainz]
# User agent for MusicBrainz API (required by their ToS)
user_agent = "kyoku/0.1.0 (https://github.com/yourname/kyoku)"

# Rate limiting (MB requires max 1 req/sec)
rate_limit_ms = 1100

[acoustid]
# AcoustID API key (get one at https://acoustid.org/new-application)
api_key = ""

[ui]
# TUI color scheme. 4 built-in themes.
# Dark:  "tokyo-night", "kanagawa"
# Light: "tokyo-night-light", "kanagawa-lotus"
theme = "tokyo-night"

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

#### Theme Data Structure

Each theme is a Rust struct with named color slots. No hex strings in UI code — always reference semantic names.

```rust
pub struct Theme {
    // Backgrounds
    pub bg: Color,              // Main background
    pub bg_alt: Color,          // Alternating rows, secondary panels
    pub bg_highlight: Color,    // Hover/focus highlight
    pub bg_selected: Color,     // Selected row

    // Borders
    pub border: Color,          // Default borders
    pub border_bright: Color,   // Focused/active borders

    // Text
    pub fg: Color,              // Primary text
    pub fg_dim: Color,          // Secondary/muted text
    pub fg_muted: Color,        // Tertiary text, placeholders

    // Semantic colors
    pub accent: Color,          // Primary accent (links, active tab, focused element)
    pub accent_alt: Color,      // Secondary accent (collections, alternate highlights)
    pub green: Color,           // Success, verified, MB match accepted
    pub yellow: Color,          // Warning, manual status, edits
    pub red: Color,             // Error, deletion, strikethrough in diffs
    pub cyan: Color,            // Info, matched status
    pub orange: Color,          // Inbox count, attention
}
```

The agent should define all 4 themes as `const` values in a `src/tui/themes.rs` module. Theme selection happens at startup from config, with no runtime overhead.

---

## 6. Feature Specifications

### 6.1 Import (`kyoku import <path>`)

Import **reads and catalogs** files into the database. It does NOT move, rename, or reorganize files. Moving files to your library structure is a separate deliberate action via `kyoku organize`.

The import pipeline:
```
Scan → Read Tags → Fingerprint (optional) → Match (MB) → Review → Write Tags → Update DB
```

Note: the files stay where they are. Their current paths are recorded in the database.

#### 6.1.1 Scan Phase
- Recursively walk `<path>` using `walkdir`
- Filter by supported audio extensions: `.mp3`, `.flac`, `.ogg`, `.m4a`, `.wav`, `.wma`, `.ape`, `.opus`
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

#### 6.1.3 Fingerprint Phase (optional, network required)
- Decode audio using `symphonia` → PCM samples
- Generate chromaprint via `rusty-chromaprint`
- Query AcoustID API with fingerprint + duration → get MusicBrainz recording IDs
- Cache fingerprints locally in the database for future dedup

#### 6.1.4 Match Phase
- For each album candidate, query MusicBrainz for matching releases
- Matching strategies (in order of preference):
  1. **MBID match**: If files already have MusicBrainz IDs in tags, fetch directly
  2. **AcoustID match**: Use fingerprint results to find release
  3. **Text search**: Search MB by artist + album + track count + duration
- Score matches by similarity (weighted combination of):
  - Artist name similarity (fuzzy string matching)
  - Album title similarity
  - Track count match
  - Total duration match (within tolerance)
  - Track title similarity (ordered comparison)
- Present top N candidates to user with similarity scores
- **Multiple releases of the same album** (e.g. US vs UK vs Japan editions): show all of them as separate candidates. Do not auto-select or filter by region. Let the user choose which edition they want.

#### 6.1.5 Review Phase (TUI or CLI)
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

#### 6.1.6 Apply Phase
- Write tags to files using `lofty` (if `write_tags = true` and user accepted a match)
- Insert/update records in SQLite database (file_path points to CURRENT location)
- Files are NOT moved or renamed — they stay exactly where they are
- Log all tag modifications for potential undo

#### CLI Flags
```
kyoku import [path]
    (no path)               Import from all configured inbox_dirs
    --pretend / -p          Dry run: show what would happen without modifying anything
    --auto / -a             Auto-accept matches above threshold, skip below
    --no-fingerprint        Skip AcoustID fingerprinting
    --no-match              Skip MusicBrainz matching entirely (import as-is with existing tags)
    --collection <name>     Add all imported tracks to a collection (creates it if needed)
    --loose                 Treat all files as individual tracks, don't try to group into albums
```

#### Handling Niche / Unmatched Content

MusicBrainz won't have entries for a lot of music: doujin releases, Bandcamp-only artists, field recordings, bootlegs, niche netlabels, etc. The import pipeline handles this gracefully:

1. **No match found** → offer "Import as-is" as the default action (not "skip")
2. **Low-confidence matches** → show them but don't pre-select; clearly label confidence
3. **`--no-match` flag** → skip MB entirely; useful for bulk-importing known-niche content
4. **`--loose` flag** → don't try to infer album structure; each file is independent
5. Unmatched tracks appear in all views, searches, and operations identically to matched ones
6. Tag status shows `unmatched` or `manual` — these are informational labels, not errors

#### 6.1.8 Future import wizard enhancements
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

If no template is set, the collection is purely organizational (a virtual grouping in the DB) and its tracks use the global template when organized.

**Import directly to collection:**
```bash
# Quick and dirty — skip MB, no album grouping, just catalog into collection
kyoku import ~/Downloads/random-mp3s/ --loose --collection "Unsorted"

# Best of both worlds — match MB for good tags, but organize into collection folder
kyoku import ~/東方/ --collection "Touhou"
```

**Tagging and organization are independent.** The `--collection` flag controls *where files end up* on disk (via the collection's path template during `kyoku organize`). MB matching controls *what the tags say*. You can combine them freely:

| Command | Tags | Filesystem layout |
|---------|------|-------------------|
| `kyoku import ~/東方/` | MB-matched | Global template (`Artist/Album/...`) |
| `kyoku import ~/東方/ --collection "Touhou"` | MB-matched | Collection template (`Collections/Touhou/...`) |
| `kyoku import ~/東方/ --loose --collection "Touhou"` | As-is from files | Collection template (`Collections/Touhou/...`) |
| `kyoku import ~/東方/ --no-match` | As-is from files | Global template (`Artist/Album/...`) |

### 6.5 Tag Editing (TUI only)

Tag editing is done through the TUI tag editor view. Select a track or album, open the editor, modify fields inline. All edits are reflected in both file tags and database. A preview diff is shown before applying changes.

### 6.6 File Organization (`kyoku organize`)

The deliberate "make my filesystem beautiful" command. This is where files actually move.

`kyoku organize` applies the `path_template` to move/copy files into the target structure under `music_dir`. It always shows a preview first and requires explicit confirmation.

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

When calculating the target path for a file, kyoku checks templates in this order:

1. **Single-disc album template** (`path_template_single_disc`) — if the track is in an album with `disc_total == 1`
2. **Global template** (`path_template`) — the default for album tracks and loose tracks

#### Collection + Album: Dual-File Behavior

**A track can exist in both an album and a collection with a template.** When this happens, `kyoku organize` creates **two physical copies** of the file:

1. The **album copy** goes to the album hierarchy via the global template: `~/Music/DJ Shadow/Endtroducing..... (1996)/01 Best Foot Forward.mp3`
2. The **collection copy** goes to the collection folder via its template: `~/Music/Collections/Touhou/IOSYS - Marisa Stole the Precious Thing.mp3`

Both copies are tracked in the database. The collection copy is created via filesystem copy (not move). This means the user's collection folders are real, self-contained directories that a file-browser music player can browse independently from the album hierarchy.

For tracks that belong **only** to a collection (loose tracks with no album), there is only one copy — in the collection folder.

For tracks in collections **without** a custom template, the collection is purely virtual (a DB grouping). No extra copies are created; the track lives in whatever location the global template dictates.

If a track belongs to multiple collections with templates, it gets a copy in each collection folder.

The organize preview shows all copies that will be created:
```
~/Downloads/東方/IOSYS - Marisa Stole the Precious Thing.mp3
  → ~/Music/IOSYS/Marisa Stole the Precious Thing.mp3            (album)
  → ~/Music/Collections/Touhou/IOSYS - Marisa Stole the Precious Thing.mp3  (collection: Touhou)
```

#### Empty Directory Cleanup
After organize moves files out of their source directories, empty directories are **automatically deleted**. This applies to any directory that becomes empty as a result of the organize operation.

#### music_dir Creation
If `music_dir` does not exist when `kyoku organize --apply` is run, kyoku **asks for confirmation** before creating it. It does not create it silently or error out.

#### Workflow
1. Calculate target paths from current tags + resolved template (see priority above)
2. For tracks in collections with templates that also belong to albums, calculate both paths
3. Show full diff: `current path → target path(s)` for every file, labeling album vs collection copies
4. User reviews, can exclude individual files
5. On confirmation: move/copy files, update DB paths, delete empty source directories
6. Files already in the correct location are skipped

The TUI equivalent: select tracks/albums, press `o` to organize, review the diff, confirm.

### 6.7 Library Relocation (`kyoku relocate`)

Rebase all library paths when you move your music to a different drive or directory. No manual SQLite queries needed.

```bash
kyoku relocate /old/Music /new/drive/Music           # Rebase all paths from old to new prefix
kyoku relocate /old/Music /new/drive/Music --pretend  # Preview changes
kyoku relocate --verify                                # Check all DB paths exist on disk, report missing
```

This does a simple string prefix replacement on all `file_path` entries in the database. Combined with `--verify`, it can also detect and report files that have gone missing (e.g. removed drive, deleted files).

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
  s                  Sort (cycle: artist, album, year, tracks)
  c                  Switch to collections view

Album Detail:
  /                  Filter tracks in this album
  e                  Edit tags (tag editor)
  R                  Rename album
  r                  Re-match against MusicBrainz (milestone 4)
  m                  Move/rename files (milestone 5)
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

### Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `{artist}` | Track artist | `Radiohead` |
| `{album_artist}` | Album artist (falls back to track artist) | `Radiohead` |
| `{artist_sort}` | Sort name (e.g. "Radiohead, The" → "Radiohead") | `Radiohead` |
| `{album}` | Album title | `OK Computer` |
| `{year}` | Release year | `1997` |
| `{title}` | Track title | `Paranoid Android` |
| `{track}` | Track number | `2` |
| `{disc}` | Disc number | `1` |
| `{genre}` | Primary genre | `Alternative Rock` |
| `{ext}` | File extension (lowercase, no dot) | `flac` |
| `{label}` | Record label | `Parlophone` |
| `{collection}` | Collection name (for collection templates) | `Touhou` |

### Format Specifiers

- `{track:02}` → `02` (zero-padded to 2 digits)
- `{disc:0}` → `1` (no padding, just the number, omitted for single-disc)
- `{year:4}` → `1997`

### Sanitization Rules

1. Replace `/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|` with `_`
2. Trim leading/trailing whitespace and dots
3. Collapse multiple consecutive spaces/underscores
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
- macOS uses NFD normalization for filenames; Linux typically uses NFC. Normalize to NFC when comparing paths, preserve original form when operating.
- Use the `unicode-normalization` crate for NFC/NFD conversion.
- Japanese filenames may contain fullwidth characters (Ａ vs A) — normalize to halfwidth for path operations, preserve in tags.
- **WSL note**: When music files live on a Windows NTFS mount (`/mnt/c/...`), filenames are case-insensitive and certain characters (`:`, `*`, `?`, etc.) are forbidden. The sanitization rules in the path template engine (Section 7) already handle this. The agent should test path operations against both native Linux paths and `/mnt/c/` paths.
- Test filenames with characters from: Japanese (hiragana/katakana/kanji), Chinese (simplified/traditional), Korean (hangul), Polish (ą, ć, ę, ł, ń, ó, ś, ź, ż), Nordic (å, ä, ö, ø), and mixed scripts.

### Search
- FTS5 `unicode61` tokenizer handles CJK by tokenizing each character individually, which works for substring-style search. `remove_diacritics 2` maps accented characters (ą→a) while keeping the original indexed too.
- Search for `初音` should match `初音ミク`. Search for `bjork` should match `Björk`.
- Fuzzy matching (used in TUI search bar) must be Unicode-aware. The `fuzzy-matcher` crate handles this. Do not assume ASCII or byte-level matching.

### TUI Display
- Use `unicode-width` crate to calculate display widths. CJK characters are typically 2 columns wide. Table column alignment must account for this.
- Never truncate in the middle of a multi-byte character or grapheme cluster.
- Use `unicode-segmentation` crate for grapheme-cluster-aware truncation.
- Test TUI rendering with mixed Latin + CJK content in the same table row.

### Sorting
- Use ICU-aware collation for sorting when possible. At minimum, case-insensitive sorting that handles Unicode correctly.
- Consider the `icu_collator` crate or simpler approaches like lowercasing with `str::to_lowercase()` (which handles Unicode case folding correctly in Rust).
- Sort names: respect sort-order tag fields from files (e.g. `ARTISTSORT = "ハツネミク"` for kana-based sorting of Japanese content).

### Crate Dependencies for Unicode

| Crate | Purpose |
|-------|---------|
| `unicode-normalization` | NFC/NFD normalization for path comparison |
| `unicode-width` | Display width calculation (CJK = 2 columns) |
| `unicode-segmentation` | Grapheme-cluster-aware string operations |

---

## 9. Error Handling Strategy

### Error Types (thiserror)

```rust
#[derive(Debug, thiserror::Error)]
pub enum KyokuError {
    #[error("File not found: {path}")]
    FileNotFound { path: PathBuf },
    
    #[error("Unsupported audio format: {ext}")]
    UnsupportedFormat { ext: String },
    
    #[error("Tag read error for {path}: {source}")]
    TagRead { path: PathBuf, #[source] source: lofty::LoftyError },
    
    #[error("Tag write error for {path}: {source}")]
    TagWrite { path: PathBuf, #[source] source: lofty::LoftyError },
    
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    
    #[error("MusicBrainz API error: {0}")]
    MusicBrainz(String),
    
    #[error("AcoustID error: {0}")]
    AcoustId(String),
    
    #[error("Template error: {message}")]
    Template { message: String },
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Configuration error: {0}")]
    Config(String),
}
```

### Principles
- Never panic on user data (malformed tags, missing fields, bad filenames)
- Collect errors per-file during batch operations, report summary at end
- Network errors should be retryable with backoff
- Tag write failures must not leave files in a half-written state (write to temp, then atomic rename)

---

## 10. Implementation Roadmap

### Milestone 1: Foundation (Core + basic read-only operations)
**Goal**: You can scan files and read/display their tags.

- [ ] Project scaffolding (Cargo.toml, module structure, error types)
- [ ] Configuration loading (TOML parsing, XDG paths, defaults, inbox_dirs)
- [ ] Database schema + migrations (rusqlite, `migrations/001_initial.sql`)
- [ ] Tag reader abstraction over `lofty` (read all supported formats)
- [ ] `kyoku info <path>` — display file metadata and tags
- [ ] `kyoku setup` — interactive config wizard
- [ ] `kyoku paths` — show resolved config/data/cache paths
- [ ] Basic test fixtures (short silence clips with known tags, including CJK-tagged files)

### Milestone 2: Library Database + Import
**Goal**: You can import files into a database and query them.

- [ ] Import files into SQLite (scan → read tags → insert, files stay in place)
- [ ] `kyoku import <path>` (local-only, no MB matching yet)
- [ ] `kyoku import --loose` and `--no-match` modes
- [ ] `kyoku scan` — inbox directory scanner (checks configured inbox_dirs)
- [ ] Search with FTS5 (freeform + filters) — TUI only
- [ ] Collections (create, add, list, show, remove, delete) — TUI only
- [ ] `kyoku import --collection` integration

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

- [ ] MusicBrainz text search (artist + album)
- [ ] Match scoring algorithm (fuzzy string comparison + metadata heuristics)
- [ ] Import wizard MB matching integration (TUI and CLI)
- [ ] Tag writing (write matched data back to files via lofty)
- [ ] Tag editing — TUI only (inline field editing in tag editor view)
- [ ] Chromaprint fingerprinting (symphonia → rusty-chromaprint)
- [ ] AcoustID lookup integration
- [ ] `--pretend` mode for all mutating commands

### Milestone 5: File Organization + Library Management
**Goal**: Beautiful filesystem output, with full user control.

- [ ] Path template engine (variables, formatting, sanitization, conditionals)
- [ ] `kyoku organize` — preview + apply file reorganization
- [ ] `kyoku organize` TUI integration (select → preview diff → confirm)
- [ ] `kyoku relocate` — rebase all paths when library moves
- [ ] `kyoku relocate --verify` — detect missing files
- [ ] Atomic file operations (temp file → rename)
- [ ] Clean up empty directories after moves

### Milestone 6: Polish & Robustness
**Goal**: Production-quality for daily use.

- [ ] Batch operations (select multiple albums in TUI)
- [ ] Performance optimization for large libraries (lazy loading, virtual scrolling, pagination)
- [ ] Comprehensive error recovery (partial import resumption)

### Milestone 7: Device Sync
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

## 12. Agent Instructions

> These instructions are for the AI coding agent that will implement this application.

### General Principles
1. **Implement one milestone at a time.** Complete all items in a milestone before moving to the next.
2. **Write tests alongside code.** Every public function should have at least one test. Run tests after each significant change.
3. **Use the type system.** Prefer enums over strings for known value sets. Use `Option` for nullable fields. Use newtypes for IDs.
4. **Handle errors explicitly.** Use `?` propagation with context. Never `unwrap()` in library code (only in tests and `main.rs` with proper error display).
5. **Keep modules focused.** One responsibility per module. If a module exceeds ~300 lines, consider splitting.
6. **Generate project tooling files** as part of Milestone 1 scaffolding: `.mise.toml` for Rust version pinning, `justfile` for task running (see Section 13). All dev commands should be discoverable via `just --list`.

### Coding Standards
- Follow `cargo fmt` and `cargo clippy` (with `#![warn(clippy::all)]`)
- Use `/// Documentation comments` on all public items
- Prefer `&str` over `String` in function parameters where ownership isn't needed
- Use `impl AsRef<Path>` for path parameters
- Async only where needed (network I/O). Internal operations are sync.
- Log with `tracing` crate (structured logging, DEBUG level for development)

### How to Implement the Import Pipeline
This is the most complex feature. Break it down:

1. **Scanner**: Takes a path, returns `Vec<ScannedFile>` (path + basic metadata)
2. **TagReader**: Takes `ScannedFile`, returns `ImportCandidate` (all tag data)
3. **AlbumGrouper**: Takes `Vec<ImportCandidate>`, returns `Vec<AlbumCandidate>` (grouped by directory + tags)
4. **Matcher**: Takes `AlbumCandidate`, queries MB, returns `Vec<MatchResult>` (scored candidates)
5. **DiffGenerator**: Takes `AlbumCandidate` + selected `MatchResult`, returns `TagDiff` (field-by-field changes)
6. **Applier**: Takes `TagDiff` + user confirmation, writes tags + moves files + updates DB

Each step is a separate function that can be tested independently.

### Database Guidelines
- Use prepared statements for all queries (no string interpolation)
- Wrap multi-step operations in transactions
- FTS5 triggers should auto-update on INSERT/UPDATE/DELETE
- Schema version tracking for future migrations

### TUI Architecture
- Use the **component pattern**: each view is a struct implementing `Widget` + handling its own events
- App state machine: `enum AppView { Library, Collections, AlbumDetail, CollectionDetail, Import, Editor, Sync, Help }`
- Separate input handling from rendering (process events → update state → render)
- Debounce search input (don't query DB on every keystroke, wait 150ms)
- The TUI is the primary interface — it should feel polished and complete, not an afterthought bolted onto a CLI tool
- Inbox indicator: on startup, run a lightweight scan of `inbox_dirs` to show pending file count

### Explicit Behavior Rules

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

---

## 13. Development Environment

### Prerequisites
- [mise](https://mise.jdx.dev/) — manages tool versions (Rust, etc.)
- [just](https://just.systems/) — command runner for project tasks
- No other system dependencies (pure Rust stack, rusqlite uses `bundled` feature)
- Runs on macOS, Linux, and Windows (WSL)

### Setup

```bash
# Install mise (if not already installed)
curl https://mise.run | sh

# Clone and setup
git clone https://github.com/yourname/kyoku.git
cd kyoku
mise install          # Installs Rust toolchain from .mise.toml
just setup            # Install dev dependencies, run initial checks
```

### .mise.toml

```toml
[tools]
rust = "1.94.1"
```

### justfile

```just
# Default: list available tasks
default:
    @just --list

# Setup dev environment
setup:
    cargo fetch
    cargo build
    @echo "Ready. Run 'just run' to launch kyoku."

# Build the project
build:
    cargo build

# Build release binary
release:
    cargo build --release

# Run kyoku TUI (default, no args)
run *ARGS:
    cargo run -- {{ARGS}}

# Run all tests
test:
    cargo test

# Run tests with output visible
test-verbose:
    cargo test -- --nocapture

# Lint with clippy
lint:
    cargo clippy --all-targets -- -W clippy::all

# Format code
fmt:
    cargo fmt

# Check formatting without modifying
fmt-check:
    cargo fmt --check

# Run all checks (lint + format + test)
check: fmt-check lint test

# Run with debug logging
debug *ARGS:
    RUST_LOG=debug cargo run -- {{ARGS}}

# Dry-run import on a path
import-preview PATH:
    cargo run -- import --pretend {{PATH}}

# Scan inbox directories
scan:
    cargo run -- scan

# Show info about a file
info PATH:
    cargo run -- info {{PATH}}

# Clean build artifacts
clean:
    cargo clean
```

### Workflow

```bash
just                   # List all available tasks
just run               # Launch TUI
just run import ~/Music/new/   # Import command
just test              # Run tests
just check             # Full CI check (fmt + lint + test)
just debug             # Run with RUST_LOG=debug
just import-preview ~/Downloads/album/  # Dry-run import
```
