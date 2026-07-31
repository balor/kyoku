# Design: external-player playback (`p` / `kyoku play`)

Status: **implemented** (branch `feature/external-player`). The normative
parts should be folded into `kyoku-spec.md` (§6 features, §7 keybindings,
§5 config reference) — tracked as a follow-up.

Implementation deltas from this proposal:
- Transport is *derived from the argv template* (any `{files}`/
  `{files-csv}` placeholder ⇒ FileList) instead of an explicit table
  field — custom `[player] command`s self-describe.
- `P` on Library/Collections behaves identically to `p` for now
  (reserved for enqueue).
- The optional setup-wizard `[player]` step was skipped (auto-detect
  covers it); noted here per §7.
- Detection probes run through a `Probes` trait so tests can fake a
  machine; spawn E2E uses a fake-player shell script
  (`tests/play_e2e.rs`).

## 1. Goal & product framing

Let the user play an album, a collection, a track, or a multi-selection
with one keypress, by handing a file list to an **external music player**
chosen by the user (or auto-detected). Linux and macOS.

kyoku stays true to "not a music player": there is no decoder, no seek
bar, no volume control in the TUI. This feature is a *hand-off* — kyoku
is the librarian, the external app is the player. The README's "What
kyoku isn't" section needs one line amended on release:

> Not a music player — no *built-in* playback; kyoku hands files to your
> external player (`p` in the TUI, `kyoku play` in the CLI).

Non-goals (v1): enqueue/append to a running player's queue, now-playing
state, MPRIS/player IPC, gapless ordering guarantees beyond the playlist
we hand over, Windows support.

## 2. UX: entry points & keybindings

`p` = "play the natural unit here", `P` = "play the larger scope".
Multi-select (Space marks) always wins over the single-row default —
consistent with how `d` already treats marks.

| View | `p` | `P` |
|---|---|---|
| Library | marked albums → all their tracks, album order; else album under cursor | — (same as `p`; reserved for future enqueue) |
| Album detail | marked tracks in view order; else track under cursor | whole album in disc/track order |
| Loose tracks | marked tracks; else track under cursor | all loose tracks (visible order) |
| Collections | marked collections; else collection under cursor | — |
| Collection detail | marked tracks; else track under cursor | whole collection in collection order |

Editor, import wizard, and popups get no binding (wrong contexts).
`p`/`P` are currently free in all four browse views — verified against
`keybindings.rs` and every view's `handle_key`.

Notices follow the existing per-view `notice` pattern:
success → `Playing: Yorushika — 幻燈 (14 tracks) via mpv`;
partial → `Playing 11 tracks (3 skipped: files missing) via VLC`;
failure → `No player found — install mpv/vlc or set [player] command in config`.

CLI (the advertised scriptable surface):

```sh
kyoku play --album "幻燈"        # exact NOCASE match; ambiguity exits 2 with a list
kyoku play --collection "Mix"
kyoku play ./some/file.flac     # positional path passthrough
kyoku play --album "幻燈" --dry-run   # prints the player argv + playlist, spawns nothing
```

## 3. Content resolution: what files, in what order

One builder per scope in the new core module (§7), all returning
`Vec<PlayItem>` where `PlayItem = { path: PathBuf, title: String,
artist: Option<String>, duration_ms: Option<u64> }` (the EXTINF fields).

- **Album** — `queries::get_album_tracks` (already `ORDER BY disc_number,
  track_number`). Use `tracks.file_path`.
- **Collection** — `queries::get_collection_tracks` (already applies
  effective collection positions — for a mixtape, order *is* the point).
  File choice per track: the **collection copy**
  (`collection_file_path`) when set and present on disk, else the primary
  `file_path`. This matches the collection-detail footer badge logic
  ([copy] → copy, [linked]/[inbox] → primary) and what `OpenDir` shows.
- **Marked items** — filtered through the view's *current visible order*
  (not `Selection::ids()` numeric order): play order should match what
  the user sees. Views already compute `filtered_indices()`.
- **Missing files** — filter `!path.exists()` out of the list, count
  them, and surface the count in the notice. All-missing → error notice,
  nothing spawned. (No prune side effects — playback never mutates the
  DB or filesystem beyond writing its own playlist file.)

## 4. Transport: how files reach the player

Two transports, chosen per player by a table field (§5):

**Playlist transport (default)** — write an M3U8 playlist, hand the
player that one path. Works for every auto-detected player and for the
OS default handlers.

- Format: `#EXTM3U` header; per item
  `#EXTINF:<seconds>,<artist?> - <title>` then the **absolute path** on
  its own line. LF newlines.
- **`.m3u8` extension, UTF-8, always.** Plain `.m3u` is latin-1 by
  convention in several players; CJK titles/paths (a core project
  concern) are only safe in m3u8.
- Location: `config::paths::cache_dir()/play/kyoku-play.m3u8`, created
  with `create_dir_all` on first use. **Fixed filename, overwritten every
  play** — no unbounded cache growth. Deliberately *not* deleted after
  spawn: mpv and friends read playlist files lazily, so the file must
  outlive the spawn. The cache dir is exactly the right lifetime.
- A literal `\n` inside a filename would corrupt the line-based format.
  The DB already guarantees UTF-8 paths (SYS-2); a newline in a filename
  is pathological — skip such entries and count them with the missing
  files in the notice.

**File-list transport** — pass the files directly as argv
(`player file1 file2 …`). Used by table entries that can't load playlist
files (Amberol, Quod Libet) and by the single-track case (no playlist
needed — every handler copes with a bare audio file). ARG_MAX is a
non-issue (Linux ~2 MB, macOS 256 KB; 500 paths ≈ 50 KB).

## 5. Player resolution

Precedence: `[player].command` → `[player].app` (macOS only) →
auto-detect → OS default-handler fallback.

```toml
[player]
# Full argv template. {playlist} is replaced with the m3u8 path;
# if no placeholder is present, the playlist path is appended last.
# command = ["mpv", "--playlist={playlist}"]
#
# macOS only: launched as `open -a <app> <playlist-or-file>`.
# app = "IINA"
```

Auto-detect tables below, in order (first hit wins). Detection is a
`PATH` probe on Linux; on macOS both a `PATH` probe (mpv/VLC ship CLI
binaries via Homebrew) and an app-bundle existence check
(`/Applications/<App>.app`, `~/Applications/<App>.app` — no `mdfind`,
no `osascript`; cheap and deterministic). Every entry was verified
against primary sources on 2026-07-19 (man pages, app docs,
`Info.plist`s — see §13); the syntax shown is what the source documents.

### Linux (PATH probe)

| binary | argv for a playlist `<P>` | source of truth | notes |
|---|---|---|---|
| mpv | `mpv --playlist=<P>` | man page synopsis `mpv [options] [file\|URL\|PLAYLIST\|-]` | opens own window per spawn; no single-instance by default |
| vlc | `vlc <P>` | man page (items are files/URLs) | `--playlist-enqueue` exists for future enqueue; new window per spawn |
| celluloid | `celluloid <P>` | README: "files/URIs as command line arguments", playlist files auto-expanded | GTK mpv frontend |
| haruna | `haruna <P>` | KDE mpv frontend, accepts files/URLs | |
| strawberry | `strawberry --load <P>` | man page: `-l/--load` replace playlist, `-a/--append`, `-p/--play`, `-c/--create`, `-k/--play-track` | single-instance (D-Bus) — second call forwards to the running window; **best-behaved library player** |
| clementine | `clementine --load <P>` | man page: same option family as Strawberry (its fork parent) | single-instance; less maintained than Strawberry — kept below it |
| audacious | `audacious -E <P>` | man page: `-E/--enqueue-to-temp` loads into "Now Playing" **and starts playback** | single-instance; plain `-e` is enqueue-without-play (future enqueue flag) |
| deadbeef | `deadbeef <P>` | `deadbeef --help` (no published man page) — `--queue` appends | loads m3u/pls; single-instance; smoke-test in WP-1 |

### macOS (PATH probe first, then app-bundle probe)

| app | launched as | source of truth | notes |
|---|---|---|---|
| mpv | `mpv --playlist=<P>` (Homebrew binary) | as Linux | |
| IINA | `open -a IINA <P>` | **`Info.plist` declares `m3u`, `m3u8`, `pls` as a "Playlist" document type** — verified in-tree | mpv-based, Music Mode; opens in current or new window per its own pref |
| VLC | `open -a VLC <P>` | registers m3u types; CLI also at `VLC.app/Contents/MacOS/VLC` | |
| foobar2000 | `open -a foobar2000 <P>` | foobar2000.org/mac — actively maintained (v2.25+, macOS 11+) | the playlist-native player; handles m3u/m3u8/pls/fpl |
| Swinsian | `open -a Swinsian <P>` | swinsian.com — maintained (v3), AppleScript-controllable | library app: imports the playlist *by reference* (no file copying) |
| Cog | `open -a Cog <P>` | github.com/losnoco/Cog — maintained (kode54) | ⚠ **sandboxed**: plays only under user-granted folders; if the music dir isn't granted, playback silently fails |
| Music | `open -a Music <P>` | Apple Support mus3081 | ⚠ **last resort** — see below |

Fallbacks: Linux → `xdg-open <P>`; macOS → `open <P>` (system default
handler for `.m3u8`).

### File-list transport entries (argv, no m3u)

| platform | binary | argv | source of truth |
|---|---|---|---|
| Linux | amberol | `amberol <files…>` | GNOME README — plays files, "does not manage playlists" (no m3u parsing) |
| Linux+mac | quodlibet | `quodlibet --enqueue-files=<f1,f2,…>` | Quod Libet command manual — also `--play-file=<f>` for singles |

### Deliberate exclusions (with reasons — verified 2026-07-19)

- **Terminal-UI players (cmus, mocp, ncmpcpp, musikcube)**: spawned from
  inside kyoku's alternate-screen TUI they would take over the terminal.
  Still usable via explicit `command`, e.g. `["kitty", "-e", "cmus"]`.
- **Rhythmbox**: the main binary takes *no* file arguments;
  `rhythmbox-client <files>` **imports files into its library database**
  — an unacceptable side effect for a play action.
- **Lollypop**: CLI options are playback-control only (play/pause/next/
  prev/"Play ids") — no way to hand it files.
- **GNOME Music**: no usable CLI surface.
- **Doppler (macOS)**: library app; opening files *imports them into its
  own library* (same objection as Music.app, no passive-play mode).
- **VOX (macOS)**: current app is centered on its own cloud/library;
  passive m3u handling unverified — dropped from auto table, still
  usable via `app = "VOX"`.
- **Colibri, Pine Player (macOS)**: passive playlist handling
  unverified — dropped for v1, revisit with hardware access.
- ⚠ **Music.app is last on the macOS table, below every third-party
  app**: Apple Support documents that *by default* "Music places a copy
  of each audio file in the Music folder … and leaves the original file
  in the current location" — i.e. opening a playlist can **duplicate the
  user's library** unless they disabled "Copy files to Media folder when
  adding to library". It stays as final fallback (it's the only
  guaranteed-present player) with the caveat documented in config
  comments and the README.

### Single-instance & window behavior (matters for repeat `p` presses)

- **Single-instance (D-Bus)**: strawberry, clementine, audacious,
  deadbeef, quodlibet — a second `p` forwards into the running window
  and replaces/enqueues per the argv we pass. `--load`-style flags make
  repeat plays predictable.
- **New window per spawn**: mpv, vlc, celluloid, haruna — pressing `p`
  twice opens two windows. Acceptable v1; enqueue (future) needs mpv IPC.
- **macOS `open -a`**: forwards to the running instance; window behavior
  is the app's own preference (IINA: "open files in current/new window").

## 6. Spawn mechanics & errors

Follow the existing `open_directory` precedent (`src/tui/views/detail.rs`):
`Command::new(argv[0]).args(&argv[1..])`, stdin/stdout/stderr all
`Stdio::null()`, `.spawn()`, **never `.wait()`** — the player must
outlive kyoku and never block the UI thread. A `spawn()` error (binary
vanished between detect and spawn, `open -a` failing) becomes the "No
player found / failed to launch" notice with the attempted argv in the
log.

Auto-detect is probed fresh on each play (players get
installed/uninstalled; the probe is ~10 `PATH` stats, negligible next to
process spawn). No caching, no invalidation bugs.

## 7. Code layout

New module `src/core/player.rs` (core, not TUI — the CLI uses it too):

```rust
pub struct PlayItem { path, title, artist, duration_ms }
pub enum PlayerKind { Argv(Vec<String>), MacApp(String) }  // resolution result

pub fn album_items(conn, music_dir, album_id) -> Result<Vec<PlayItem>>
pub fn collection_items(conn, music_dir, collection_id) -> Result<Vec<PlayItem>>
pub fn items_from_tracks(rows: impl Iterator<Item = TrackRow>) -> Vec<PlayItem>  // marked/loose

pub fn resolve_player(settings: &Settings) -> Option<PlayerKind>   // config → detect → fallback
pub fn write_playlist(items: &[PlayItem], cache_dir: &Path) -> Result<PathBuf>
pub fn play(settings: &Settings, items: Vec<PlayItem>) -> Result<PlayOutcome>

pub struct PlayOutcome { pub player_label: String, pub played: usize, pub skipped_missing: usize }
```

- `play` owns the single-file-vs-playlist branch, the write, the spawn,
  and the skip counting. TUI views and the CLI only assemble items and
  render the outcome into a notice/stdout.
- `resolve_player` and the detect tables are pure functions over an
  injectable "path exists" / "app bundle exists" probe so tests can fake
  a machine with only mpv, only Music.app, or nothing.
- `open_directory` stays where it is; unifying the two spawn sites into a
  `core::launch` helper is a separate, optional refactor — don't bundle.

TUI wiring (all mechanical, but every site listed so none drift):
`keybindings.rs` adds `is_play`/`is_play_scope`; the four views' `handle_key`;
`handlers.rs` for any cross-view action; **status bars in all four
views** and **the help overlay** (TUI-9 was "help drift" — these two
surfaces are part of the feature, not an afterthought).

CLI: new `Command::Play { album, collection, path, dry_run }` in
`src/cli/mod.rs`, dispatched in `main.rs` after settings load; album
lookup exact-NOCASE with an ambiguity list on multiple hits (exit 2).

Setup wizard (`src/cli/setup.rs`): optional — a `[player]` step offering
the detected list (default: auto). Skippable for v1; note it in the
commit either way.

## 8. Testing

- **Playlist serialization** (unit): EXTINF escaping (`,` and newlines
  in titles), CJK round-trip, missing duration → `-1` or omit per M3U
  convention (decide, test it), path with spaces/UTF-8 written raw.
- **Resolution** (unit): precedence command > app > detect > fallback;
  `{playlist}` placeholder present vs appended; macOS table order puts
  Music last — all via injected probe fakes.
- **Content builders** (integration, in-memory DB + tempdir): album
  order across discs; collection copy-vs-primary choice (copy exists →
  copy; copy recorded but file gone → primary; both gone → skipped
  count); marked-tracks view-order ordering.
- **Spawn E2E** (Linux CI): prepend a temp dir to `PATH` containing a
  fake `mpv` shell script that records argv to a file; `play()` 3 items;
  assert argv shape and playlist contents. macOS `open -a` can't run in
  Linux CI — cover by keeping every macOS-specific decision in the pure
  probe functions.
- **Keybinding smoke** (existing TUI test style): `p` on an empty album
  produces a notice, not a panic.

## 9. Docs to update on implementation

- `README.md` — feature bullet + the "What kyoku isn't" amendment (§1)
  + a `[player]` config example.
- `kyoku-spec.md` — §6 features, §7 keybinding table, §5 config
  reference, and the §6.4 collections section ("a collection is a
  playlist you can now actually play").
- Help overlay + four status bars (see §7).
- `justfile`/CI — nothing new; spawn tests must skip gracefully when
  `sh` is unavailable (never an issue on Linux/macOS CI).

## 10. Implementation work packages

- **WP-1 · core** — `core/player.rs`: items builders, m3u8 writer,
  resolver + detect tables (with per-entry transport), `play()`.
  Unit + integration tests. No UI. **Smoke-test on real machines:**
  deadbeef argv (no published man page), Cog sandbox grant flow,
  Swinsian import-vs-play behavior, Amberol/Quod Libet file-list argv.
- **WP-2 · TUI** — keybindings, four views, status bars, help overlay,
  notices. Fake-player E2E.
- **WP-3 · CLI & docs** — `kyoku play`, optional setup step, README +
  spec fold-in. Sequencing: WP-1 → WP-2 ∥ WP-3.

## 11. Future (explicitly out of scope v1)

- **Enqueue/append** (`P` in Library/Collections is reserved for this).
  Verified per-player flags from this research: VLC `--playlist-enqueue`,
  Strawberry/Clementine `-a/--append` (+`-p/--play`), Audacious
  `-e/--enqueue` (running instance), DeaDBeeF `--queue`, Quod Libet
  `--enqueue-files`, mpv `--playlist-append` (needs an IPC-configured
  instance to be useful). Table gains an `enqueue_argv` field when this
  lands.
- **Now-playing indicator / MPRIS** on Linux, `osascript` player state
  on macOS — a status-bar line, not a player UI.
- **Per-collection player overrides** (audiobook player for audiobook
  collections, e.g. `collections.player`).
- **Flatpak/snap detection** — `flatpak run <id>` works from a plain
  spawn; detecting installed flatpaks needs `flatpak list --app`
  parsing. v1 probes `PATH` only.

## 13. Research log (2026-07-19)

Primary sources consulted while building the §5 tables:

- mpv man page (mankier mirror) — `PLAYLIST` positional, `--playlist=`,
  `--playlist-append`.
- VLC man page (mankier) — items/files; `--playlist-enqueue`.
- Strawberry man page (mankier) — full player/playlist option set.
- Clementine man page (Debian manpages) — same option family; M3U/XSPF/
  PLS/ASX import-export listed.
- Audacious man page (mankier) — `-e` vs `-E` semantics (enqueue vs
  enqueue-and-play).
- rhythmbox + rhythmbox-client man pages — no file args on main binary;
  client imports into library → excluded.
- Lollypop man page (Arch manual) — playback-control options only →
  excluded.
- Celluloid README — files/URIs as CLI args; playlist files
  auto-expanded.
- Haruna GitHub (KDE) — libmpv frontend, files/URLs.
- DeaDBeeF GitHub README — active, nightly Linux/Windows/**macOS**
  builds; CLI from `--help` (no man page) → smoke-test in WP-1.
- Quod Libet command manual — `--play-file`, `--enqueue-files`,
  `--add-location` (latter imports → not used).
- Elisa (KDE apps page) — maintained; positional files presumed (`%U`),
  m3u unverified → kept off v1 table.
- Tauon Music Box GitHub + manual — Linux+macOS, playlist-oriented;
  CLI surface undocumented → kept off v1 table.
- Amberol README — files only, no playlist management.
- Sayonara site — maintained (1.11.0, 2025); CLI surface undocumented →
  off v1 table.
- IINA GitHub README + `iina/Info.plist` (develop branch) — Music Mode,
  CLI tool, and **`m3u`/`m3u8`/`pls` registered as Playlist document
  type**.
- Swinsian site — v3 maintained, AppleScript control, library model.
- Cog GitHub (losnoco/kode54) — maintained; App Sandbox permission
  caveat from the README addenda.
- foobar2000.org/mac — stable + preview channels, macOS 11+.
- VOX site — cloud/library-centric; passive m3u unverified → excluded.
- Doppler (brushedtype.co) — library-import model → excluded.
- Apple Support mus3081 — Music.app copies imported files into
  `~/Music/Music` **by default** (verbatim) → Music last resort.

## 12. Open questions for the owner

1. `P` semantics: "larger scope" (proposed) vs. "enqueue" from day one?
2. Should `kyoku play --album` with multiple NOCASE hits list-and-exit
   (proposed) or play the first with a warning?
3. Fixed playlist filename (proposed: `kyoku-play.m3u8`, zero cache
   growth) vs. per-play timestamped files (enables a crude "recent
   playlists" later)?
