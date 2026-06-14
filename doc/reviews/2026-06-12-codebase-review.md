# Full codebase review — 2026-06-12

Deep review of the entire codebase at commit `e13f447` (~24k lines of Rust).
Baseline at review time: build clean, 175 tests passing, ~20 minor clippy warnings.

This is a **living tracking document**: after each fix lands, update the matching
finding's `Status` (and add the commit hash). Statuses: `open`, `in-progress`,
`fixed (<commit>)`, `wontfix (<reason>)`, `obsolete`.

Finding IDs are stable — reference them in commit messages (e.g. `fix(organizer): ... [ORG-1]`).

**Step-by-step fix instructions for every open finding live in
[2026-06-12-fix-instructions.md](2026-06-12-fix-instructions.md)**, grouped
into self-contained work packages (WP-A … WP-I) with suggested sequencing.
Pick up a package from there; this file stays the findings index / status
tracker.

---

## Verdict

Well-built project: clean core/db/tui/external split (TUI never touches files
directly), the relative-path refactor is consistent and well-tested, path safety
floors (`remove_empty_parents`) are genuinely careful, MusicBrainz rate limiting
is compliant, no SQL injection anywhere, real test suite. But: **one critical
data-loss scenario, several genuine file/DB-loss bugs, and two systemic habits**
(error swallowing, lossy path conversion) that undermine the care taken elsewhere.

---

## Critical

### ORG-1 — Unmounted/missing `music_dir` wipes the entire DB on organize
**Status:** fixed (240157a) · **Confidence:** certain
`plan_organize` classifies any track whose file fails `exists()` as an orphaned
row (`src/core/organizer.rs:188`), and `apply_organize` unconditionally deletes
those rows (`src/core/organizer.rs:580-584`). An unmounted NAS/drive — or a
merely unreadable dir, since `exists()` returns false on EACCES — turns *every*
track into a "missing source". The CLI makes it worse: when `music_dir` doesn't
exist it offers "Create it? [y/N]" then applies against the fresh empty dir
(`src/main.rs:423-438`), guaranteeing a full prune. Preview caps the missing
list at 10 entries, so a 5,000-row wipe reads as 10 lines + "…and 4990 more".
Files survive; albums/collections/ordering/MB matches are destroyed.
**Fix plan:** (a) require `read_dir(music_dir)` to succeed at plan time, move the
CLI create-prompt before planning; (b) `prune_blocked_reason` on the plan when
missing ≥5 && >20% of planned tracks, or ≥100 flat — apply skips the prune loop
and surfaces the reason; (c) re-check `exists()` per row at apply time, report
prune errors instead of `.is_ok()`-swallowing.

---

## High

### ORG-2 — Organize moves silently overwrite untracked files at the destination
**Status:** fixed (240157a) · **Confidence:** certain
Collision avoidance consults only DB paths, never disk; `fs::rename`/`fs::copy`
clobber on POSIX (`src/core/organizer.rs:437-450`, covers `:540-552`). Reachable
via kyoku's own flows: delete-rows-keep-files leaves files at canonical paths;
re-import + organize renames the new files straight over them. Move targets can
also collide with another track's `collection_file_path`, which isn't in
`used_paths` (only `tracks.file_path` is seeded, `organizer.rs:130`).
**Fix plan:** disambiguate against disk (with an exemption for `file_orphans`
paths — dup-replace intends those overwrites); seed `used_paths` with collection
paths; final dest-exists guard at apply (canonicalize-equality exemption for
case-only renames). Covers keep overwrite behavior by design.

### ORG-3 — Order-dependent slot collision destroys one track's audio
**Status:** fixed (240157a) · **Confidence:** likely
The "already in place" branch claims a path without checking whether a moving
track already reserved it (`src/core/organizer.rs:262-283`). When two tracks
render the same template path, SQL row order decides between correct
disambiguation and: mover's file renamed over the in-place track's file, then
`update_track_path` fails on UNIQUE, leaving the mover's row pointing at its
now-empty source — pruned as missing on the next organize. Net: one audio file
lost, one DB row lost.
**Fix plan:** two-pass planning — pass 1 renders primary targets and pre-reserves
all in-place slots; pass 2 assigns move targets. Regression test in both row orders.

### DB-1 — Tag editor poisons `track_number`/`disc_number` as TEXT
**Status:** fixed (a7ffd13) · **Confidence:** likely
`update_track_fields` binds every value as `&str` (`src/db/queries.rs:844-855`);
the editor mirrors typed values straight through (`src/tui/views/edit.rs:529-538`).
SQLite INTEGER affinity stores `3/12` or `""` as TEXT; `map_track_row`'s
`Option<u32>` get then returns `InvalidColumnType`, failing `get_album_tracks`
for the whole album until hand-fixed. Fix: parse to integer/NULL before binding.

### DB-2 — `.ok()` on `query_row` conflates DB errors with "not found" (~10 sites)
**Status:** fixed (a7ffd13) · **Confidence:** certain
`src/db/queries.rs:70, 127, 176, 190, 566, 826, 883, 1058, 1430, 1451`. A
`SQLITE_BUSY`/IO error reads as "no row": `get_or_create_album` then creates
duplicate albums; `find_track_by_mbid`/`find_track_by_album_slot` make dup
detection silently miss; `add_to_collection.rs:84` treats `Err` as "create new
collection". Fix: `rusqlite::OptionalExtension::optional()` throughout.

### DB-3 — No `busy_timeout` on two concurrent write connections
**Status:** fixed (a7ffd13) · **Confidence:** likely
Import worker opens its own connection on a background thread while the TUI
thread writes through another; neither sets `busy_timeout` (`src/db/schema.rs:16-18`,
`src/tui/views/import/worker.rs:32`), so collisions return `SQLITE_BUSY`
immediately — then get swallowed by DB-2's `.ok()`s. Fix: `busy_timeout(5s)` at
open. (Pairs with DB-2; one PR kills the class.)

### TUI-1 — `j`/`k` can't be typed into popup text inputs or global search
**Status:** fixed (930419f) · **Confidence:** certain
`is_up`/`is_down` match bare `k`/`j` (`src/tui/keybindings.rs:24-30`) and are
checked before `TextInput::handle_key` in `global_search.rs:90-100`,
`add_to_collection.rs:49-61`, `pick_collection.rs:74-86`. "Jazz", "J-Pop",
"junjou" can't be typed. The standalone create/rename inputs forward keys
directly, so behavior is inconsistent between inputs.

### TUI-2 — Import wizard hard-locks if the scan/import worker thread dies
**Status:** fixed (930419f) · **Confidence:** likely
`tick` treats `Disconnected` like `Empty` (`src/tui/views/import/render.rs:18-44`);
Scanning/Importing aren't in `can_cancel()` (`import.rs:394-402`); `q`/Ctrl+C
suppressed in the import view (`app.rs:141`); raw mode blocks SIGINT — only
`kill` escapes. Amplifier: MB threads `client.lock().unwrap()` (`wizard.rs:401,585,756`)
— one panic poisons the mutex for all later searches. Fix: treat `Disconnected`
as failed completion; allow cancel from Scanning/Importing.

### TUI-3 — Duplicate-resolver decisions silently wiped by a late release fetch
**Status:** fixed (930419f) · **Confidence:** certain
`tick` re-runs `refresh_conflict_preview` when a fetch lands and `is_in_summary()`
— which is still true during `ImportStep::ResolveDuplicates`
(`src/tui/views/import/render.rs:111-114`, `wizard.rs:460-462`). All keep/replace
decisions reset, cursor jumps to conflict 1. Fix: guard must also require
`step == ImportStep::Review`.

### EXT-1 — Malformed config.toml bricks `kyoku setup` and `kyoku paths`
**Status:** fixed (6575900) · **Confidence:** certain
`Settings::load(&config_path)?` runs before command dispatch (`src/main.rs:71`),
so the recovery commands die on a TOML error too. Contradicts spec rule 18.
Fix: dispatch Setup/Paths before loading settings.

### EXT-2 — MusicBrainz disc/medium info flattened away
**Status:** fixed (6575900) · **Confidence:** likely
`parse_full_release` merges all media keeping per-disc positions
(`src/external/musicbrainz.rs:733-770`); `MbTrack` has no medium field. On
multi-disc releases: local disc-2 track 1 can greedily claim the disc-1 MB track
(`worker.rs:456-554`); accepted matches write wrong `TrackNumber` and never
`DiscNumber`. Comment at `dup_detect.rs:440-444` shows the flattening already
shipped one bug. No multi-disc tests despite spec §11 listing them.

---

## Systemic themes

### SYS-1 — Error swallowing after destructive steps
**Status:** fixed (065fe19)
Beyond DB-2: `apply_delete_plan` discards promote/detach failures *after* files
are deleted (`src/core/pruner.rs:279-286`); collections organize handler drops
both `Err` and `result.errors` (`src/tui/app/handlers.rs:233-239`), nullifying
`apply_organize`'s careful "file moved but DB update failed" reporting;
`rename_album`/`create_collection` results `.ok()`ed in the TUI; import worker
`.ok()`s the release re-fetch and silently commits the group without MB metadata
*after* executing destructive replace decisions (`worker.rs:85-90, 209-233`).
Fix as policy: audit every `let _ =` / `.ok()` / `if let Ok` touching DB or fs.

### SYS-2 — Non-UTF-8 paths corrupted system-wide
**Status:** fixed (e5a3306) · **Confidence:** certain (mechanism)
All path persistence goes through `display().to_string()` (`src/core/paths.rs:22-41`,
every row mapper, `importer.rs:204`), replacing invalid bytes with U+FFFD. Old
Shift-JIS/GBK rips (the stated CJK use case) import "successfully" with a phantom
path — then ORG-1's machinery deletes the row on the next organize. Principled
fix: store path bytes (beets does). Pragmatic fix: refuse to import any path
where `to_str()` fails, making the failure loud. Related: `disambiguate` maps a
non-UTF-8 stem to `""` → `" (2).mp3"` (`organizer.rs:152-161`).

### SYS-3 — Inconsistent twins (same job, divergent behavior; one of each pair has the bug)
**Status:** open
- `apply_delete_plan` (continues on error) vs `apply_delete_collection_with_roots`
  (aborts mid-way via `?` *after* files are deleted, `organizer.rs:863-876`).
- `PickCollectionPopup` (has "create new with typed name") vs `AddToCollectionPopup`
  (can't create "Jazz" while "Jazz Classics" exists, `add_to_collection.rs:73-91`).
- CLI organize (no confirmation despite help text + spec §6.6) vs TUI organize
  (preview + Enter) — `src/main.rs:422-446`. Spec also documents `--pretend`,
  which doesn't exist.
- CLI importer (transaction-wrapped inserts) vs TUI worker (auto-commit per
  statement; dup-replace delete-then-insert non-atomic, `worker.rs:205-233`).
- Three collection create/lookup paths with divergent case sensitivity and no
  UNIQUE constraint (`queries.rs:189-204, 801-804, 882-891`; unconditional
  `create_collection` at `collections.rs:169`).

### SYS-4 — Repetition already showing drift
**Status:** partial (e5a3306; shared path safety, list cursor/filter, reload-preserve, HTTP plumbing; column SELECT consts and organize-popup extraction remain)
- Organize-popup key handling copy-pasted 4× (`library.rs:181-221`,
  `detail.rs:237-335`, `collections.rs:119-157`, `collections.rs:727-765`) —
  already differ on Esc scroll reset.
- List navigation duplicated 6× (library/detail/collections/collection-detail/
  editor/global-search); editor page size (10) already differs from the rest (20).
- "Reload preserving cursor" repeated 6× in `handlers.rs` — carries TUI-6's bug
  in every copy.
- `path_is_in_roots` implemented 3× (`organizer.rs:891-895`, `:923-925`,
  `pruner.rs:72-76`) — this is the safety floor for every destructive op; belongs
  in `core::paths` with the invariant documented once.
- ~90 lines of throttle/retry plumbing duplicated between MB and CAA clients
  (CAA copy lost `error_chain()`, so its network errors log poorly).
- Six hand-repeated 10-column track SELECT lists + three 11-column album lists
  that `map_track_row`/`map_album_row` depend on positionally (`queries.rs:428,
  527, 546, 591, 708, 829` / `:363, 479, 568`) — a reorder in one copy silently
  shifts `Option` columns into each other. `EXISTING_TRACK_SELECT` (`:1415`)
  already demonstrates the fix.
- `filtered_indices`/`set_filter` duplicated verbatim (`detail.rs:80-100`,
  `collections.rs:622-642`).
- Cross-device copy+delete fallback duplicated for audio + covers
  (`organizer.rs:445-449`, `:547-551`).

---

## Medium

### DB-4 — Migrations neither atomic nor re-runnable; mid-chain failure bricks the DB
**Status:** fixed (b06f8a2) · `src/db/schema.rs:16-45`, `migrations/002_fts_triggers.sql`
`user_version` stamped once at the end; `execute_batch` not transactional. A
mid-chain failure re-runs earlier migrations on next open → "trigger already
exists" forever. Fix: stamp per-step inside transactions. Also guard against
down-stamping a newer DB (`schema.rs:42` writes unconditionally).

### DB-5 — FTS `album_title` goes permanently stale after album rename
**Status:** fixed (b06f8a2) · `queries.rs:864-870, 1087-1103`, `src/tui/mod.rs:39-45`
Triggers only on `tracks`; rebuild only runs when FTS is completely empty.
`rename_album` (result `.ok()`ed at `detail.rs:353`) leaves search matching the
old title.

### CORE-1 — `organize_operation` is a free string; typo silently degrades copy→move
**Status:** fixed (8693f47) · `src/config/settings.rs:55`, `organizer.rs:442,544`
`if operation == "copy" else move` — `"Copy"`, `"cp"`, any typo moves files the
user asked to copy. Make it a serde enum (pattern exists in the same file).
Also unvalidated: `auto_match_threshold` (>1.0 silently disables auto-accept),
`rate_limit_ms` (0 disables MB throttling).

### CORE-2 — Copy mode repoints DB at the copy; original becomes re-importable
**Status:** accepted / not planned · `organizer.rs:442-456`
`update_track_path` rewrites to the destination even in copy mode, so the kept
original drops out of known-paths: `scan_inbox` re-surfaces it, re-import creates
a second row, next organize copies it again with " (2)". Each cycle can duplicate
the library.

Owner decision: copy mode intentionally leaves the original eligible for later
import; adding a separate source-path tracker is not worth the added model
complexity.

### CORE-3 — Importer groups by directory only; first track's album tag stamps the group
**Status:** fixed (e010ebe) · `src/core/importer.rs:94-123, 304-345`
Mixed folders get welded into one wrong album even though per-file album tags
sit in `tag_data_map` (the "we don't have album on Track" comment is stale).
`ordered_group_indices` already computes a per-item `album_key` the grouping ignores.

### CORE-4 — Promotion runs even when `delete_files=false`; collection promote skips disk check
**Status:** fixed (8693f47) · `pruner.rs:277-286`, `organizer.rs:770-773, 790-809`
With files kept, the primary file is stranded untracked (not orphan-tracked).
Collection deletion promotes to any non-NULL `collection_file_path` with no
`.exists()` check (album deletion checks, `pruner.rs:167-172`); `.ok()` also
swallows query errors into "no candidate".

### EXT-3 — Re-running `kyoku setup` discards customizations; writes dead `user_agent` key
**Status:** fixed (6575900) · `src/cli/setup.rs:130-156, 233-301`
inbox_dirs not offered back; templates/thresholds/`write_tags` reset to
hardcoded defaults. Generated config contains `user_agent` (settings has no such
field; serde drops it) with a stale version and a URL differing from the
compiled-in constant (`musicbrainz.rs:10` says kyoku-project, setup says balor).

### EXT-4 — Wizard tiebreaker rescoring biased by hardcoded `api_score = 100`
**Status:** fixed (b06f8a2) · `wizard.rs:647-665`, `musicbrainz.rs:787`
Real search score restored *after* `new_score` is computed — refetched candidates
get a flat 1.0 on the 0.10-weight API factor exactly when candidates are near-tied.

### TUI-4 — `/` and `g` live in views with no search bar; Tab bypasses collections popups
**Status:** fixed (d090b51) · `src/tui/app.rs:176-186`, `handlers.rs:214-217`
`/` mid-import-review focuses an invisible widget that swallows all keys (view
appears frozen); `g` opens global search from inside the wizard and `switch_view`
abandons the mid-flight review. Tab in collections is checked before
`has_popup()` (unlike library/detail/collection-detail), discarding typed input.

### TUI-5 — Import worker degrades MB groups on re-fetch failure, after destructive dup decisions
**Status:** fixed (d090b51) · `worker.rs:85-90, 143, 209-233`
Worker re-fetches the full release (`.ok()`) even when the wizard already has it
(one wasted rate-limited call per group); on failure the group commits with no
MB metadata and no error — but `replace_existing` plans (computed against the MB
identity) were already applied. Fix: pass the fetched release down; surface
failures in the summary.

### TUI-6 — Filter/search-bar desync after delete, editor return, or refresh
**Status:** fixed (e5a3306) · `handlers.rs:277-313, 589-613, 661-687`
Reload paths clear the view's `filter` but not `search.value`; the restored
cursor was an index into the *filtered* list reapplied to the unfiltered one.
Fix once in a shared `reload_view_preserving_cursor` (see SYS-4).

### TUI-7 — `start_scan` walks the whole library synchronously in the key handler
**Status:** fixed (d090b51) · `wizard.rs:464-470`, `import.rs:371-374`
`music_dir` is always a scan source; `scan_inbox` (full walk + per-file DB check)
runs before the next draw — UI freezes before the Scanning screen appears.
`refresh_counts` (`handlers.rs:555-564`) repeats a smaller version after every
delete/import.

### TUI-8 — Late MB auto-accept can flip a group's action while user sits in summary
**Status:** fixed (d090b51) · `render.rs:69-84`, `wizard.rs:202-206`
Enter doesn't set `user_decided`, so a slow search result auto-selects AcceptMb
after the user moved past; if the candidate's tracklist is already populated,
no preview refresh ever fires. Either Enter counts as a decision, or any action
flip while `is_in_summary()` invalidates the preview.

### TUI-9 — Help screen drift
**Status:** fixed (d090b51) · `help.rs:152-165`
Documents `g`/`Home`/`End` list bindings that don't exist; contradicts itself on
`g` within one screen; lists `i` as global (library-only, `handlers.rs:95`).
Library/detail status bars omit working `d` and `Space`. README promises every
screen shows its keys.

### TEST-1 — FLAC/OGG tag tests have never run
**Status:** fixed (e5a3306) · `tests/tag_reader_test.rs:25-54`, `tests/fixtures/create_fixtures.rs`
Tests pass green when fixtures are missing; the generator only synthesizes MP3s.
Spec §11 promises fixtures in every supported format. Missing fixture should
fail (pattern repeated 5×, plus `import_organize_e2e.rs:22-25`).

### EXT-5 — CLI organize `--apply` lacks the confirmation its help text promises
**Status:** fixed (6575900) · `src/main.rs:422-446`, `cli/mod.rs:51`
(Also listed under SYS-3.) Spec §6.6: "always shows a preview first and requires
explicit confirmation". Only the music_dir-creation prompt exists. `--pretend`
alias documented in spec but unimplemented.

---

## Low

| ID | Finding | Where | Status |
|----|---------|-------|--------|
| L-1 | Tagger tmp files keep real audio extensions → re-imported as tracks | `tagger.rs:483-495` | fixed (8693f47) |
| L-2 | Tag *write* failures reported as `TagRead` (and point at the tmp path) | `tagger.rs:466-471` | fixed (8693f47) |
| L-3 | Cover art written non-atomically; tag editor's copy-tmp-rename pattern exists to reuse; `to_string_lossy` into DB | `detail.rs:649-652` | fixed (8693f47) |
| L-4 | LIKE patterns don't escape `%`/`_`; all-`"` FTS term is a syntax error; `fts_count` errors `unwrap_or(0)`; LIKE fallback inconsistently skips album titles | `queries.rs:463-466, 518-544, 634` | fixed (065fe19, e5a3306) |
| L-5 | No index on `tracks.mbid` (scan per track during dup detect); none on `(title, album_artist)`; `idx_tracks_path` redundant with UNIQUE | `migrations/001_initial.sql` | fixed (b06f8a2) |
| L-6 | Cross-device fallback fires on *any* rename error, masking cause — match `ErrorKind::CrossesDevices` (stable 1.85) | `organizer.rs:445-449, 547-551` | fixed (8693f47) |
| L-7 | `q` quits from a dirty editor, no prompt (import view got one for this reason) | `app.rs:140-146` | fixed (d090b51) |
| L-8 | `missing_sources` not re-checked at apply (stale plan) — folded into ORG-1c | `organizer.rs:580-584` | fixed (240157a) |
| L-9 | Setup writes unescaped paths into TOML (a `"` in a path produces the broken config that trips EXT-1); inbox entries unvalidated | `setup.rs:226-301` | fixed (6575900) |
| L-10 | All-punctuation artist → malformed Lucene query → hard error instead of fallback chain | `musicbrainz.rs:96, 114-117` | fixed (b06f8a2) |
| L-11 | `scripts_of` misses CJK Ext-B (U+20000+) — rare-kanji titles defeat the script-mismatch guard | `matching.rs:239-270` | fixed (e5a3306) |
| L-12 | Empty local artist scores 0.0 at full 0.15 weight instead of being excluded → silently disables auto-accept | `matching.rs:49-53, 289-298` | fixed (e5a3306) |
| L-13 | Duration tolerance scales with `local_track_titles.len()` not `local_track_count` (latent) | `matching.rs:137` | fixed (e5a3306) |
| L-14 | `score_release` doc weights wrong on 3 of 7 factors | `matching.rs:22-30` | fixed (e5a3306) |
| L-15 | Tiebreaker `fetch_release` failure swallowed with no log | `wizard.rs:647` | fixed (b06f8a2) |
| L-16 | `insert_track` silently drops `Track.mbid` (compensated by separate update); `channels`/`acoustid`/`chromaprint` are dead columns | `queries.rs:84-115` | fixed (b06f8a2) |
| L-17 | `initialize` can down-stamp a newer DB — folded into DB-4 | `schema.rs:42` | fixed (b06f8a2) |
| L-18 | Collection-copy lifecycle: existing-on-disk skip never backfills NULL `collection_file_path`; template change strands old copies | `organizer.rs:297-301, 339-344` | partial (8693f47; backfill fixed, template-change move remains TODO) |
| L-19 | `DeletePlan::deletable_file_count` double-counts when collection copy *is* the primary | `pruner.rs:101-137` | fixed (8693f47) |
| L-20 | Importer parses every file twice with lofty (doubles I/O on large imports) | `importer.rs:219-224` | fixed (8693f47) |
| L-21 | Year read only via `ItemKey::Year` + `parse::<u32>` — TDRC / `YYYY-MM-DD` lose the year → "Album (0000)" | `tagger.rs:76-80` | fixed (8693f47) |
| L-22 | `.expect()` on batch refs in conflict renderer — one refactor from a panic | `import/render.rs:940, 988` | fixed (d090b51) |
| L-23 | Byte-slice of album MBID would panic on non-ASCII (`chars().take(8)` is free) | `detail.rs:825` | fixed (d090b51) |
| L-24 | Resolver hint drift: comment promises `S` skip key that doesn't exist; hint hardcodes "1-5" while handler takes 1-9 and `match_candidates` is configurable | `wizard.rs:891-899`, `import/render.rs:195` | fixed (d090b51) |
| L-25 | Collection-detail organize notice built then discarded (`let _notice`) | `handlers.rs:439-444` | fixed (d090b51) |
| L-26 | Organize CLI filter flags silently non-exclusive (artist wins) — clap `ArgGroup` | `cli/mod.rs:59-72`, `main.rs:302-312` | fixed (6575900) |
| L-27 | flake.nix installs `stable.latest` while claiming to match mise's 1.94.1 | `flake.nix:31-32` | fixed (e5a3306) |
| L-28 | Doc comment for `escape_lucene` sits on `error_chain` | `musicbrainz.rs:880-884` | fixed (e5a3306) |
| L-29 | ~20 clippy warnings (`cargo clippy --fix` clears about half) | various | partial (e5a3306; backlog reduced to 4 baseline warnings) |
| L-30 | In-memory pagination / N+1 on collection paths; per-row `canonicalize` in `list_all_known_paths` at startup — fine at hobby scale, watch at 50k+ tracks | `queries.rs:21-49, 669-700, 1216-1256` | wontfix (scale) |

---

## Verified clean (checked explicitly, not absence of findings)

- Relative-path refactor round-trips correctly: component-aware strip/join,
  `Music` vs `Music Backup` boundary, symlinked-music_dir canonicalize fallback,
  v7 migration transactional and tested. `source_dir` stays absolute by design.
- `remove_empty_parents` never deletes roots, never climbs out; well tested.
- Template sanitization is char-boundary-safe and CJK-preserving; no substring
  hazard in `{album_artist}`/`{artist}` ordering. (Windows reserved device names
  unhandled — Linux-only target.)
- No SQL injection: column names whitelisted, sorts are closed enums, IN-lists
  interpolate only `?` placeholders.
- MB rate limiting compliant (1100 ms start-to-start incl. retries), proper
  versioned User-Agent on both clients, 15/20 s timeouts, correct percent-encoding,
  panic-free JSON parsing, CAA 404→None + Original→1200→500 fallback correct.
- `get_or_create_album` NULL `album_artist` handled with `IS ?2`.
- FK behavior matches code assumptions (collection_tracks CASCADE, tracks NO ACTION).
- Dup-replace orphan-overwrite guard (`occupied` set, literal+canonical) correct
  and regression-tested.
- `collection_order.rs` cohesion heuristics internally consistent and tested.
- `organize_preview.rs` dangling/absorbed orphan split consistent (tested).
- Weight redistribution in `score_release` normalizes correctly when factors
  are excluded.

---

## Suggested order of attack

1. **ORG-1/2/3** — the data-loss bugs in the product's core operation (in progress).
2. **DB-1 + DB-2 + DB-3** — one short PR: `.optional()` sweep, `busy_timeout`,
   integer parsing in `update_track_fields`. Kills a whole silent-corruption class.
3. **TUI-2, TUI-3, TUI-1** — the worst UX traps (lockup, lost decisions, blocked typing).
4. **SYS-1** as a standing audit, then the SYS-4 extractions (which fix TUI-6 and
   several mediums as a side effect).
5. Everything else opportunistically, mediums before lows.
