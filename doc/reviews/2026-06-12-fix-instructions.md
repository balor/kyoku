# Fix instructions for open review findings

Companion to [2026-06-12-codebase-review.md](2026-06-12-codebase-review.md).
Each section below is a **self-contained work package** an agent can pick up
without re-deriving the analysis. Read the matching finding in the review
report first for the full evidence; this file tells you *how* to fix.

Line numbers drift — locate code by **function/symbol name**, not by the line
numbers quoted in the review report.

## Conventions (apply to every package)

- Reference finding IDs in the commit message: `fix(scope): ... [EXT-1]`.
- Add a regression test for every behavioral fix (house style: in-file
  `#[cfg(test)]` modules; organizer/pruner use `fresh_world()` helpers in
  `src/core/organizer_tests.rs`).
- Verify with `cargo test` (must stay at 0 failures) and `cargo clippy
  --all-targets` (don't add warnings; baseline is ~19 lib / 20 bin).
- After landing, update the finding's `Status:` in the review report to
  `fixed (<commit>)`.
- Do not commit unless asked; never add AI co-author credits.
- Already-fixed context you can rely on: `plan_organize` refuses missing
  music_dir and blocks mass prunes (ORG-1/2/3, `240157a`); all `query_row`
  lookups use `.optional()?` and connections set `busy_timeout(5s)`
  (DB-1/2/3, `a7ffd13`); the import wizard detects dead worker threads and
  `j`/`k` are typeable in inputs (TUI-1/2/3, `930419f`).

---

## WP-A · CLI & config bootstrap — EXT-1, EXT-5, L-26, L-9, EXT-3

Do EXT-1 first; CORE-1 (WP-D) makes config parsing stricter and depends on
recovery commands working with a broken config.

### EXT-1 — malformed config bricks `kyoku setup`/`kyoku paths` (HIGH)
In `src/main.rs`, `Settings::load(&config_path)?` runs before the command
dispatch, so a TOML syntax error kills the recovery commands too.
1. Restructure `main`: handle `Some(Command::Setup)` and `Some(Command::Paths)`
   (and the `None`-with-no-config welcome message) **before** calling
   `Settings::load`. The `needs_config` check already exists — mirror its
   exemption list.
2. When `Settings::load` fails for the remaining commands, print the config
   *path* and a hint to run `kyoku setup` along with the parse error.
3. Verify manually: write `not valid toml` into the config, confirm
   `kyoku setup` and `kyoku paths` still run and `kyoku scan` prints the
   helpful error.

### EXT-5 — `kyoku organize --apply` lacks its promised confirmation (MEDIUM)
In `src/main.rs` `Command::Organize` arm, after the plan is printed and before
`apply_organize`:
1. Prompt `Apply these changes? [y/N]` (reuse the existing stdin prompt
   pattern used for music_dir creation in the same arm).
2. Add a `--yes`/`-y` flag in `src/cli/mod.rs` to skip the prompt — the CLI is
   advertised as scriptable, so a non-interactive path is mandatory.
3. Spec §6.6 mentions `--pretend` as a dry-run alias: add a no-op `--pretend`
   flag with `conflicts_with = "apply"`, or delete the mention from
   `kyoku-spec.md`. Either is fine; say which you did in the commit.

### L-26 — organize filter flags silently non-exclusive
In `src/cli/mod.rs`, put `--artist/--album/--path/--collection` into a clap
`ArgGroup` with `multiple = false` so passing two is a hard error instead of
"artist silently wins" (`src/main.rs` if-else chain).

### L-9 — setup writes unescaped strings into TOML
`src/cli/setup.rs` interpolates paths into `"{}"` TOML strings; a `"` or `\`
in a path produces an invalid config (which then used to trip EXT-1).
Preferred fix: stop hand-formatting — build a `Settings` value and serialize
with `toml::to_string_pretty` (Settings already derives `Serialize`). You lose
the hand-written comments in the generated file; preserve the important ones
by emitting them as a header comment block above the serialized body.
Also validate wizard-entered inbox dirs (tilde-expand + existence check) the
same way music_dir/db_dir are validated.

### EXT-3 — re-running setup discards customizations; dead `user_agent` key
In `src/cli/setup.rs`:
1. When an existing config was loaded, carry the full `Settings` through the
   wizard and only overwrite fields the wizard actually asked about
   (music_dir, db_dir, name_script, cover size, theme). Today
   `path_template*`, `auto_match_threshold`, `match_candidates`, `write_tags`,
   `show_cover_preview` are silently reset to defaults. Doing L-9's
   serialize-the-struct rewrite makes this nearly free.
2. Offer existing `inbox_dirs` back as pre-confirmed entries instead of
   starting the list empty.
3. Delete the `user_agent = ...` line from the generated config — the field
   doesn't exist in `MusicBrainzSettings`, serde drops it, and the real UA is
   the compile-time constant in `musicbrainz.rs` (whose URL also disagrees
   with the one setup writes).

---

## WP-B · MusicBrainz multi-disc & scoring — EXT-2, EXT-4, L-15, L-10

### EXT-2 — disc/medium info flattened away (HIGH)
Root cause: `parse_full_release` in `src/external/musicbrainz.rs` merges all
media into one `tracks` vec keeping per-disc `position`; `MbTrack` has no
medium field.
1. Add `pub disc: u32` to `MbTrack` (1-based medium index) and
   `pub medium_count: u32` to `MbRelease`. Set both in `parse_full_release`
   by iterating media with their index.
2. In `src/tui/views/import/worker.rs` `match_group_to_mb`: when
   `medium_count > 1`, match on `(local disc_number, track_number)` against
   `(mt.disc, mt.position)` instead of position alone. Keep the title-based
   fallback pass as-is. For single-medium releases keep current behavior.
3. In `build_mb_tag_changes` (same file): when a match is accepted and
   `medium_count > 1`, also emit a `DiscNumber` tag change from `mbt.disc`
   (don't write DiscNumber for single-disc releases — avoid churning files).
4. Revisit the workaround documented at the comment in
   `src/tui/views/import/dup_detect.rs` (~line 440, "phantom dup conflicts"):
   it papers over exactly this flattening. With real disc info it can likely
   be tightened, but only change it with a test reproducing the phantom-dup
   scenario first.
5. Tests: `musicbrainz.rs` has JSON-string parse tests — add a two-medium
   release fixture asserting `disc`/`medium_count`. Add a worker-level test
   that a local "disc 2 track 1" does not claim the disc-1 MB track.
   Spec §11 lists multi-disc as a required fixture case.

### EXT-4 — tiebreaker rescoring biased by hardcoded api_score
In `src/tui/views/import/wizard.rs`, the tied-leader refine block inside the
search thread (search for `preserved_api`): the code assigns
`cand.release = full` (whose `api_score` is the hardcoded 100 from
`parse_full_release`), computes `score_release`, and only *then* restores the
real api_score. Reorder: restore `cand.release.api_score = preserved_api`
**before** calling `score_release`. Three-line move; add a comment.

### L-15 — tiebreaker fetch failure swallowed silently
Same block: the `if let Ok(full) = client.fetch_release(&mbid)` drops the
error. Add an `else`/match arm with
`tracing::warn!("tied-leader refine fetch {} failed: {}", mbid, e)` to match
every other MB call site.

### L-10 — all-punctuation artist → malformed Lucene query
In `musicbrainz.rs` `search_releases`, the **first** query pass builds
`artist:(...)` from `strip_trailing_punct(artist)` without checking for empty
(the later fallbacks do check). Guard pass 1 with
`!clean_artist.trim().is_empty()`; when empty, skip straight to the fallback
that omits the artist clause. Test: `search` query-building unit test with an
artist of `"..."` must not produce `artist:()`.

---

## WP-C · Schema & migrations — DB-4, DB-5, L-5, L-16, collections UNIQUE (from SYS-3)

Order matters: do DB-4's restructure first, then add the new migration (v8)
for the rest.

### DB-4 — migrations neither atomic nor re-runnable (MEDIUM)
In `src/db/schema.rs::initialize`:
1. Wrap **each** `apply_vN` in its own transaction and stamp `user_version`
   to N inside it, instead of stamping once at the end:
   ```rust
   fn run_step(conn: &Connection, n: i32, f: impl Fn(&Connection) -> Result<()>) -> Result<()> {
       let tx = conn.unchecked_transaction()?;
       f(conn)?;
       set_schema_version(conn, n)?;
       tx.commit()
   }
   ```
   Caveat: `apply_v7` already opens its own internal transaction — lift that
   out (the wrapper now provides it) so transactions don't nest.
2. Guard against future DBs: read `user_version` first; if it is **greater**
   than `SCHEMA_VERSION`, return a `Config` error ("database schema vX is
   newer than this kyoku — upgrade kyoku") instead of running and re-stamping
   down. Only call `set_schema_version` when actually migrating.
3. Tests (in the existing `schema.rs` test module): (a) re-running
   `initialize` on a current DB is a no-op; (b) a DB stamped v99 errors;
   (c) simulate the half-migrated state — set `user_version = 1` on a fully
   built DB and confirm `initialize` fails *cleanly* (no panic) rather than
   succeeding by accident, or make `apply_v2`'s DDL idempotent
   (`IF NOT EXISTS`) and assert the rerun succeeds.

### DB-5 — FTS `album_title` stale after album rename (MEDIUM)
The fts5 table is standalone (post-`005_fix_fts_schema.sql`) with triggers on
`tracks` only. Add `migrations/007_fts_album_rename.sql` + `apply_v8`:
```sql
CREATE TRIGGER albums_fts_title_update AFTER UPDATE OF title ON albums
WHEN NEW.title IS NOT OLD.title
BEGIN
    UPDATE tracks_fts SET album_title = NEW.title
    WHERE rowid IN (SELECT id FROM tracks WHERE album_id = NEW.id);
END;
```
(Plain fts5 tables support UPDATE by rowid.) This covers both `rename_album`
and `update_album_mb`. Bump `SCHEMA_VERSION` to 8. Test: insert album+track,
rename album via `queries::rename_album`, assert the FTS search
(`search_tracks`) finds the new title and not the old one.

### L-5 — missing/redundant indexes
Same v8 migration:
```sql
CREATE INDEX IF NOT EXISTS idx_tracks_mbid ON tracks(mbid) WHERE mbid IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_albums_title_artist ON albums(title, album_artist);
DROP INDEX IF EXISTS idx_tracks_path;  -- redundant: file_path is UNIQUE
```

### Collections UNIQUE + one canonical lookup (part of SYS-3)
Three divergent paths exist: `get_or_create_collection` (case-sensitive),
`find_collection_by_name` (NOCASE), `create_collection` (no check, called
unconditionally from `collections.rs` create flow).
1. Migration (v8 or v9): dedupe existing case-insensitive duplicate names
   first — keep the lowest id, repoint `collection_tracks.collection_id`,
   delete the others (`INSERT OR IGNORE` then fix-ups, or do it procedurally
   in the `apply_vN` Rust fn, which is easier to get right). Then rebuild
   `collections` with `name TEXT NOT NULL UNIQUE COLLATE NOCASE` (SQLite
   can't add a constraint in place: create new table, copy, drop, rename).
2. Code: make `get_or_create_collection` the single entry point, matching
   `COLLATE NOCASE`; have `create_collection` callers go through it (the
   collections-view `n` flow must surface "already exists" instead of
   silently succeeding).
3. Test: creating "mix" when "Mix" exists returns the existing id.

### L-16 — `insert_track` drops `Track.mbid`
Add `mbid` to the INSERT column list in `queries::insert_track` (the worker's
separate `update_track_mb` then becomes redundant for new inserts but is
harmless — leave it). Leave the dead `channels`/`acoustid`/`chromaprint`
columns alone; just note them. Test: insert a track with `mbid: Some(...)`,
read it back.

---

## WP-D · Core file-ops & importer — CORE-1, CORE-2, CORE-3, CORE-4, L-6, L-18, L-19, L-20, L-21, L-1, L-2, L-3

### CORE-1 — `organize_operation` free string; typo = destructive move (MEDIUM)
1. In `src/config/settings.rs`, replace `pub organize_operation: String` with
   ```rust
   #[derive(..., Serialize, Deserialize, Default)]
   #[serde(rename_all = "lowercase")]
   pub enum OrganizeOperation { #[default] Move, Copy }
   ```
   (follow the existing `CoverArtSize`/`NameScriptPreference` enum patterns
   in the same file).
2. Change `apply_organize(.., operation: &str, ..)` to take the enum; update
   call sites: `main.rs`, `tui/app/handlers.rs` (×3), `tui/views/detail.rs`.
   The string comparisons `operation == "copy"` become `matches!(op, OrganizeOperation::Copy)`.
3. While in `Settings::load`, add post-parse validation with warnings:
   clamp `auto_match_threshold` to `0.0..=1.0`, floor `rate_limit_ms` at
   1000 (MB policy is 1 req/s) — log via `tracing::warn!`, don't error.
4. Note: an invalid `organize_operation` value now fails config parse — this
   is why EXT-1 (recovery commands working with broken config) must land
   first.

### CORE-2 — copy mode repoints DB at the copy; original re-importable (MEDIUM)
**Needs a design decision from the owner before implementing.** The two
coherent designs:
- (a) *Library copy is canonical* (current pointer behavior, recommended):
  keep `update_track_path` to the destination, but record the source path so
  it stops re-surfacing. Add a small `imported_sources(path TEXT PRIMARY
  KEY)` table written on copy-mode organize and on import; include it in
  `list_all_known_paths` so `scan_inbox` skips those files.
- (b) *Original is canonical*: in copy mode skip `update_track_path`
  entirely; the plan already skips when the destination exists, so re-runs
  are idempotent. Downside: the organized tree contains files the DB doesn't
  point at, and delete/retag flows operate on the original.
Present both to the owner; implement (a) unless told otherwise. Test: import
→ organize in copy mode → `scan_inbox` must return empty.

### CORE-3 — importer groups by directory only (MEDIUM)
In `src/core/importer.rs::group_into_albums`: the per-file album tag is
available in `tag_data_map` (the "we don't have album on Track" comment is
stale — delete it). Change the group key from `source_dir` to
`(source_dir, normalized_album_tag)` where the normalization is
trim+lowercase, falling back to the directory name when the album tag is
empty. `ordered_group_indices` already computes a per-item `album_key` —
reuse that logic, don't duplicate it. Then the insert phase's
`get_or_create_album` call per group is correct by construction.
Test: one directory containing files tagged Album A and Album B → two
groups / two album rows.

### CORE-4 — deletion promote/detach bugs (MEDIUM)
Three related fixes:
1. `src/core/pruner.rs::apply_delete_plan` and
   `src/core/organizer.rs::apply_delete_collection_with_roots`: skip the
   `promote_paths` loop entirely when `delete_files == false` — the primary
   file stays on disk, so repointing `tracks.file_path` at the collection
   copy strands it. (Alternatively compute promotions at apply time only
   when the file was actually deleted.)
2. `apply_delete_collection_with_roots` aborts on first DB error via `?`
   *after* files are already unlinked. Replace the `?`s on
   `update_track_path`/`delete_track`/`delete_collection` with per-row
   accumulation into `result.errors`, matching `apply_delete_plan`'s policy
   exactly. While there: `pruner.rs`'s own `let _ = queries::...` swallows in
   the promote loop must push into `report.errors` too (also listed in SYS-1).
3. `organizer.rs::find_other_collection_path`: the `.ok()` swallows real
   errors (use `.optional()?` — same pattern as DB-2) and the chosen path is
   never checked against disk. Return all candidates (drop `LIMIT 1`,
   iterate) and pick the first whose resolved path `.exists()`, mirroring
   `plan_delete_albums` (`pruner.rs`, which already does the exists check).
Tests: pruner_tests.rs has the harness; add cases for delete_files=false
(no promotion happens) and for a recorded-but-missing collection copy
(promotion picks an existing candidate or none).

### L-6 — cross-device fallback fires on any rename error
In `apply_organize` (two identical blocks: audio moves, cover moves): replace
`.or_else(|_| copy+delete)` with a shared helper in `core` —
```rust
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        r => r,
    }
}
```
(`CrossesDevices` is stable since Rust 1.85.) This stops EACCES/ENOENT from
being masked by a confusing copy error.

### L-18 — collection-copy lifecycle gaps
In `plan_organize`'s collection-copy branches (`raw.exists()` → skip):
(a) when the file exists but the DB's `collection_file_path` for that
(track, collection) is NULL or different, emit a DB-only backfill — add a
`copy_backfills: Vec<(track_id, collection_id, PathBuf)>` to `OrganizePlan`,
applied in `apply_organize` as `update_collection_track_path` calls with no
file operation. (b) When the rendered target differs from the recorded
`collection_file_path` and the old file still exists (template changed),
plan a *move* from old to new instead of a fresh copy. (b) is more invasive;
it's acceptable to land (a) alone and note (b) as a TODO in the plan struct
docs.

### L-19 — delete preview double-counts primary==copy
`pruner.rs::plan_delete_tracks`: before pushing into
`collection_copies_to_delete`, skip paths already present in
`files_to_delete` (use a `HashSet<&Path>`). Test asserts
`deletable_file_count` == 1 for a loose track whose collection copy *is* its
primary file.

### L-20 — importer parses every file twice
`src/core/importer.rs` calls `read_track(&abs_path)` then
`read_tags(file_path)` — both run `lofty::read_from_path`. Change
`core::tagger::read_track` to return `(Track, TagData)` (it already has the
parsed `TaggedFile`; `tag_data_from_tagged_file` exists for exactly this) and
update the importer to use the pair. Also note the asymmetry: `read_track`
gets the canonicalized path, `read_tags` the original — keep the
canonicalized one.

### L-21 — year lost for TDRC / "YYYY-MM-DD" values
`src/core/tagger.rs` (search `ItemKey::Year`): build a fallback chain —
`Year`, then `RecordingDate`, then `ReleaseDate`; for each value accept either
a plain `u32` parse or a 4-digit prefix (`value.get(..4)` all-ASCII-digits →
parse). Test with a tag whose year field is `"2024-03-01"`.

### L-1 / L-2 / L-3 — tagger & cover-write polish
- L-1: export the tmp-suffix marker from `tagger.rs` (e.g.
  `pub const TMP_MARKER: &str = ".kyoku-tmp"`) and make
  `importer::scan_audio_files` skip any file whose name contains it, so
  crash leftovers aren't imported as tracks.
- L-2: add a `TagWrite { path, source }` variant to `KyokuError`
  (`src/error.rs`) and use it in `tagger.rs::apply_and_save` for save
  failures, reporting the *real* target path, not the tmp path.
- L-3: `src/tui/views/detail.rs` cover fetch handler: write the downloaded
  bytes to `cover.<ext><TMP_MARKER>` then `fs::rename` into place (the tag
  editor's copy-tmp-rename pattern), and use `display().to_string()` instead
  of `to_string_lossy()` for the DB path to match the rest of the codebase.

---

## WP-E · TUI behavior — TUI-4, TUI-5, TUI-7, TUI-8, TUI-9, L-7, L-22, L-23, L-24, L-25

(TUI-6 is fixed by the shared reload helper in WP-G #3 — do it there.)

### TUI-4 — `/` and `g` active in views without search; Tab bypasses popups
In `src/tui/app.rs` `handle_key`:
1. Add `fn view_supports_search(&self) -> bool` — true only for views that
   actually render the search bar (check `app/render.rs` for which views draw
   it: Library, Collections, detail views). Gate the `is_search_focus` branch
   on it. Today pressing `/` in Import/Editor focuses an invisible widget
   that eats all keys — the view looks frozen.
2. Gate the `g` (global search) branch on `!matches!(self.view,
   AppView::Import | AppView::Editor)` — jumping out of a mid-flight import
   review bypasses its cancel confirmation.
3. In `app/handlers.rs::handle_collections_key`, move the
   `self.collections.has_popup()` check **before** `keys::is_tab_switch` —
   mirror `handle_library_key`, which guards correctly. Today Tab while the
   rename/create input is open silently discards the typed name.

### TUI-5 — worker degrades MB groups on re-fetch failure, after destructive deletes
In `src/tui/views/import/worker.rs` + `wizard.rs::start_import`:
1. The wizard usually already holds the full release for accepted groups
   (`ensure_full_release_for_group` / tied-leader refine). Pass it into the
   worker (e.g. add `mb_full: Option<MbRelease>` to the per-group plan
   struct) and only `fetch_release` in the worker when absent — saves one
   rate-limited call per group.
2. When the worker still has to fetch and the fetch **fails**: do NOT proceed
   silently. Specifically (a) skip applying that group's `replace_existing`
   plans (they were computed against the MB identity; deleting library rows
   without it is wrong), import the group as-is instead, and (b) append a
   line to the completion summary ("group X imported without MB metadata:
   fetch failed"). The current `.ok()` produces no trace at all.

### TUI-7 — `start_scan` walks the library on the UI thread
`wizard.rs::start_scan` calls `importer::scan_inbox` (full walk + per-file DB
check) synchronously before the first Scanning frame renders. Move it into
the spawned scan thread: the thread can't borrow `conn`, so open its own
connection from `db_path` (the import worker in `worker.rs` already does
exactly this, and `busy_timeout` is set since `a7ffd13`). Send an initial
`ScanMessage::Progress(0, 0)` so the UI shows "indexing…" immediately.
Also note `refresh_counts` (`app/handlers.rs`) re-walks inboxes after every
delete/import on the UI thread — acceptable for now, but worth a
"counts may lag" comment if you touch it.

### TUI-8 — late auto-accept flips a group the user already passed
In `wizard.rs`: navigating forward past a group is a decision. In
`next_group()` (and the summary-entering path), set
`group.user_decided = true` on the group being left. The auto-select branch
in `render.rs::tick` already skips `user_decided` groups, so this closes the
"slow MB result flips an already-reviewed group while I sit in the summary"
hole. Test: simulate by setting up groups, advancing past group 0, then
delivering an MB result for it — action must stay as the user left it.

### TUI-9 — help screen and status bars drift
`src/tui/views/help.rs`: (a) the "Navigation (lists)" section documents
`g/Home` and `G/End` — `g` opens global search (same screen says so eight
lines up) and Home/End are unhandled. Either implement Home/End in the shared
list-navigation helper (WP-G #2, preferred) and reword `g` out of the row, or
delete the row. (b) `i` (import) is listed as Global but only works in the
Library view — move it to a Library section. (c) Add the working `d`
(delete) and `Space` (mark) keys to the library/detail status-bar hints
(see `status_hints`/footer builders in those views). README promises every
screen shows its keys — verify against the actual handlers, not the old help
text.

### L-7 — `q` quits from a dirty editor without confirmation
`src/tui/app.rs`: `view_captures_input` is false in the editor when no field
is being edited, so `q` quits and discards changes. The editor tracks
dirtiness (footer shows "Unsaved changes" — find that flag). On quit/back
with a dirty editor, open a confirm popup (reuse the `ConfirmCancel` +
`ConfirmDelete::new(...).without_checkbox()` pattern from the import view in
`app/handlers.rs`).

### L-22 / L-23 / L-24 / L-25 — small TUI fixes
- L-22: `import/render.rs` conflict renderer: replace the two
  `.expect("batch ref must point at an existing track")` with a graceful
  fallback (skip the row / placeholder text) — the renderer already has an
  empty-state branch to mirror.
- L-23: `detail.rs`: `&mbid[..mbid.len().min(8)]` → `mbid.chars().take(8).collect::<String>()`.
- L-24: `wizard.rs` resolver comment promises an `S` (skip) key that doesn't
  exist — fix the comment; `import/render.rs` hint hardcodes "1-5" — derive
  from `match_candidates` (and the handler accepts 1-9; clamp hint text
  accordingly).
- L-25: `app/handlers.rs` collection-detail organize: `let _notice = format!(...)`
  builds the summary then drops it — assign it to the view's notice field
  (the library handler shows the pattern).

---

## WP-F · Error-swallowing audit — SYS-1 (+ remaining .ok() sites)

Policy: any `let _ =`, `.ok()`, `unwrap_or_default()`, or `if let Ok(..)`
whose failure leaves **disk and DB inconsistent, or user work silently
discarded**, must either propagate, accumulate into the operation's
`errors` vec, or set the view's notice. Best-effort swallows that are truly
fine (e.g. `create_dir_all` before a move that will itself error) get a
`// best-effort:` comment so the next audit skips them.

Sweep commands:
```
grep -rn "let _ = " src/ --include=\*.rs
grep -rn "\.ok();" src/ --include=\*.rs
grep -rn "if let Ok(" src/tui/ --include=\*.rs
```

Known offenders to fix (some overlap with WP-D/WP-E items — coordinate):
1. `src/tui/app/handlers.rs` collections-view organize apply:
   `if let Ok(_result) = apply_organize(...) {}` — drops both `Err` and
   `result.errors`. Mirror the library handler's notice composition
   (including the `prune_blocked_reason` part added in `240157a`). The
   collections view needs a notice field if it lacks one.
2. `src/core/pruner.rs::apply_delete_plan` promote/detach loop:
   `let _ = queries::update_track_path(...)`, `let _ = clear_track_album(...)`
   → push into `report.errors` (files are already deleted at that point;
   silence is the worst option). Covered in CORE-4 but listed here too.
3. `src/tui/views/detail.rs`: `rename_album(...).ok()` — on Err, set the
   view notice ("rename failed: …") and don't update the local title.
4. `src/tui/views/collections.rs` create flow: `create_collection(...).ok()`
   — surface failure; goes away if WP-C's canonical get-or-create lands.
5. `src/tui/views/import/worker.rs`: `update_album_mb(...).ok()` and the
   release re-fetch `.ok()` (TUI-5) — count into the import summary.
6. `src/db/queries.rs::fts_count`-style `unwrap_or(0)` in search: log once
   via `tracing::warn!` before falling back to LIKE (part of L-4 too).
After the sweep, re-grep and confirm every remaining swallow has a
`// best-effort:` justification comment.

---

## WP-G · Dedup extractions — SYS-4 (fixes TUI-6 and the AddToCollection bug as side effects)

Recommended order: each step is independently landable.

1. **`path_is_strictly_inside` into `core::paths`** — one function replacing
   the three identical implementations: `organizer.rs::path_is_in_roots`,
   the closure inside `organizer.rs::remove_empty_parents`, and
   `pruner.rs::path_is_managed`. Carry over the safety-invariant doc comment
   ("never a root itself, never outside all roots"). This is the safety
   floor for every destructive op — port the existing tests, don't rewrite
   them.
2. **`ListCursor` helper** (suggest `src/tui/widgets/list_cursor.rs`):
   `{ selected: usize, scroll: usize }` with methods
   `up/down/page_up/page_down/half_*/to_top/to_bottom(count)`, page size a
   const 20. Replace the six hand-rolled copies (library, detail,
   collections, collection-detail, editor, global-search). Note the editor
   currently pages by 10 — unify to 20 deliberately and say so in the commit.
   Implement `Home`/`End` here to un-lie the help screen (TUI-9).
3. **`reload_view_preserving_cursor`** in `app/handlers.rs` — single helper
   for the six "reload then restore selection" sites (Deleted ×2, refresh
   ×3, editor-return ×3). It must: reload, re-apply `self.search.value` as
   the view filter (this is the TUI-6 fix — today the filter is cleared but
   the search bar still shows the query), and clamp the cursor against the
   *filtered* list length. Add a regression test if the view structs are
   testable headless; otherwise document the manual repro (filter, delete a
   row, confirm the bar+list agree).
4. **Merge `AddToCollectionPopup` into `PickCollectionPopup`** — one widget,
   two commit modes (return-name vs commit-to-DB). Port
   `PickCollectionPopup`'s pinned "+ New: <typed>" entry; that fixes the
   bug where typing "Jazz" can't create it while "Jazz Classics" exists
   (review: tui finding 4). Keep the `j`/`k` typeability test green.
5. **Shared throttled HTTP client** in `src/external/` — extract
   `USER_AGENT`, client builder, `throttle()`, `AttemptError`,
   retry-once-with-backoff, and `error_chain()` from `musicbrainz.rs` and
   `cover_art_archive.rs` (~90 duplicated lines; the CAA copy lost
   `error_chain`, so its network errors log poorly — extraction fixes that).
   Keep per-service rate limits configurable. While there, fix L-28 (the
   `escape_lucene` doc comment sits on `error_chain`).
6. **Column-list consts in `queries.rs`** — `TRACK_ROW_COLUMNS` /
   `ALBUM_ROW_COLUMNS` strings used by every SELECT feeding
   `map_track_row`/`map_album_row` (six and three hand-repeated copies;
   `EXISTING_TRACK_SELECT` already shows the pattern). Positional drift here
   is silent because most columns are `Option`.
7. **`filtered_indices`/`set_filter`** — identical in `detail.rs` and
   `collections.rs`; extract into a small shared helper (natural home:
   wherever `ListCursor` lives).

---

## WP-H · Unicode & matching — SYS-2, L-11, L-12, L-13, L-14, L-4

### SYS-2 — non-UTF-8 paths corrupted system-wide
Full fix (byte/BLOB path storage, what beets does) is a redesign — out of
scope. Land the pragmatic containment:
1. In `importer::scan_audio_files` (and any other walkdir entry point): if
   `path.to_str().is_none()`, skip the file, `tracing::warn!` it, and count
   it; surface "N files skipped (non-UTF-8 filename — rename to import)" in
   the scan results (CLI and wizard).
2. That makes the lossy `display().to_string()` pipeline unreachable for
   non-UTF-8 inputs instead of silently corrupting them into phantom rows
   (which ORG-1's prune machinery would then delete).
3. Leave a `// SYS-2:` comment at `core::paths::to_db_path` noting the
   invariant ("callers guarantee UTF-8; scanner enforces").
Test: create a file with invalid UTF-8 bytes in the name
(`OsStr::from_bytes(b"bad\xFF.mp3")`, unix-only `#[cfg]`) and assert the
scanner skips + counts it.

### L-11 — `scripts_of` misses CJK Extension B+ and Hangul extensions
`src/external/matching.rs::scripts_of`: extend the CJK arm with
`0x20000..=0x2EBEF` (Ext B–F) and `0x30000..=0x3134F` (Ext G), and Hangul
with `0xA960..=0xA97F` (Jamo Ext-A) and `0xD7B0..=0xD7FF` (Jamo Ext-B).
Test: a title of Ext-B ideographs vs a Latin title must register as
different scripts (so the Jaro-Winkler ~0 gets neutralized, which is the
whole point of the script guard).

### L-12 — empty local artist tanks every candidate uniformly
`matching.rs::score_release`: when the local artist is empty/whitespace,
*exclude* the artist factor and redistribute its weight — exactly how the
year/tracks/duration factors already handle "unknown". Today `sim("", x) = 0`
at full 0.15 weight caps the total below `auto_match_threshold`, silently
disabling auto-accept for artist-less groups. Test: artist-less group with a
perfect-title candidate must be able to reach ≥ 0.85.

### L-13 / L-14 — matching nits
- L-13: tolerance line in `score_release`: use `local_track_count` instead of
  `local_track_titles.len()` (latent, but it's the right scale and always
  available).
- L-14: fix the doc-comment weight table above `score_release` to match the
  code (album 0.15, duration 0.05, year 0.15 — the comment is the only
  tuning "spec", and it's wrong on three factors).

### L-4 — search query edges
In `src/db/queries.rs` search functions: (a) add `ESCAPE '\'` to the LIKE
patterns and escape `%`/`_`/`\` in user input; (b) in the FTS query builder,
drop terms that are empty after stripping `"` (an all-quote term builds
`""*`, an fts5 syntax error failing the whole search); (c) replace the
`unwrap_or(0)` on the FTS count with a logged fallback; (d) make the LIKE
fallback search album titles like the FTS branch does (they currently
disagree). Tests: search for `100%`, for `"`, and for an album title via the
LIKE path.

---

## WP-I · Tests & toolchain — TEST-1, L-27, L-29, L-30

### TEST-1 — FLAC/OGG tag tests have never run
Two parts:
1. Make missing fixtures **fail** instead of silently passing: in
   `tests/tag_reader_test.rs` (5 sites) and `tests/import_organize_e2e.rs`,
   replace the early-return-if-missing with
   `panic!("fixture missing: {} — see tests/fixtures/README", path)`.
2. Provide the fixtures. `create_fixtures.rs` only synthesizes MP3 frames;
   hand-rolling FLAC/OGG is not worth it. Generate tiny real files once
   (e.g. `ffmpeg -f lavfi -i anullsrc=d=0.1 -c:a flac silence.flac`, same
   for libvorbis → .ogg, each ~1 KB), tag them with the values the tests
   expect (read the test asserts first), commit them under
   `tests/fixtures/sample_library/`, and document the regeneration commands
   in a `tests/fixtures/README.md`. Spec §11 wants every supported format
   covered.

### L-27 — toolchain drift
`flake.nix` installs `rust-bin.stable.latest` while the comment claims it
matches `.mise.toml`'s 1.94.1. Pin: `rust-bin.stable."1.94.1".default`
(keep one source of truth — read the version from `.mise.toml` is overkill;
just add a "keep in sync with .mise.toml" comment on both sides).

### L-29 — clippy backlog
`cargo clippy --fix --lib -p kyoku` + `--bin kyoku` clears about half
(the `std::slice::from_ref` and collapsible-if batches). Do the remainder by
hand; the two functions flagged "too many arguments" (9–10) are
`ImportView::start` and friends — fold the settings-derived params into a
small `ImportConfig` struct rather than suppressing the lint.

### L-30 — scale notes (no action)
In-memory collection pagination, N+1 membership loads, and per-row
`canonicalize` at startup are fine at hobby scale. Leave as `wontfix (scale)`
in the report unless libraries reach ~50k tracks; then revisit
`get_collection_tracks` (push ordering into SQL) and batch the
`list_all_known_paths` canonicalization.

---

## Suggested sequencing

1. **WP-A** (EXT-1 unblocks stricter config parsing) → then **WP-D** CORE-1.
2. **WP-C** (DB-4 restructure before any new migration; v8 carries DB-5,
   L-5, collections UNIQUE).
3. **WP-B** (multi-disc — highest-value import correctness).
4. **WP-F** + **WP-G** together (the audit names the sites the extractions
   then make impossible to get wrong; WP-G #3 fixes TUI-6).
5. **WP-E**, **WP-H**, **WP-I** in any order.
