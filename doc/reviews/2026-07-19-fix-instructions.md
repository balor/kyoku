# Fix instructions for open review findings

Companion to [2026-07-19-deep-review.md](2026-07-19-deep-review.md).
Each section below is a **self-contained work package** an agent can pick up
without re-deriving the analysis. Read the matching finding in the review
report first for the full evidence; this file tells you *how* to fix.

Line numbers drift — locate code by **function/symbol name**, not by the line
numbers quoted in the review report.

## Conventions (apply to every package)

- Reference finding IDs in the commit message: `fix(scope): ... [DEL-1]`.
- Add a regression test for every behavioral fix (house style: in-file
  `#[cfg(test)]` modules; organizer/pruner tests live in
  `src/core/organizer_tests.rs` / `src/core/pruner_tests.rs`; the review
  report's "Verification appendix" describes the probe scenarios to encode
  as permanent tests).
- Verify with `cargo test` (must stay at 0 failures) and `cargo clippy
  --all-targets` (don't add warnings; baseline is 5).
- After landing, update the finding's `Status:` in the review report to
  `fixed (<commit>)`.
- Do not commit unless asked; never add AI co-author credits.
- Context you can rely on: all June fixes in
  [2026-06-12-codebase-review.md](2026-06-12-codebase-review.md) are landed
  (migrations are per-step transactional; connections set
  `busy_timeout(5s)`; plan_organize refuses a missing music_dir and blocks
  mass prunes; wizard detects dead workers).

---

## WP-A · Search & FTS — FTS-1, L-42, L-48, L-31, SYS-3d

The flagship fix. Do FTS-1 first; the rest ride the same files.

### FTS-1 — CJK substring search (HIGH)
Root cause: spec assumption wrong — unicode61 does **not** tokenize CJK
per-character, so `"term"* `prefix queries in
`queries::search_tracks` only match at token starts.
1. New migration `migrations/008_fts_trigram.sql` + `apply_v9` in
   `src/db/schema.rs` (bump `SCHEMA_VERSION` to 9):
   ```sql
   DROP TABLE IF EXISTS tracks_fts;
   CREATE VIRTUAL TABLE tracks_fts USING fts5(
       title, artist, album_title,
       tokenize='trigram'
   );
   INSERT INTO tracks_fts(rowid, title, artist, album_title)
   SELECT t.id, t.title, t.artist, a.title
   FROM tracks t LEFT JOIN albums a ON t.album_id = a.id;
   ```
   Triggers from `002`/`006`/`007` reference `tracks_fts` by name only, so
   they survive the drop/recreate (verify — SQLite triggers on `tracks`
   and `albums` are independent of the FTS table's tokenizer).
   Trigram needs SQLite ≥ 3.34; bundled rusqlite is far newer. Note:
   trigram ignores `remove_diacritics`; diacritic folding for Latin
   (`bjork` → `Björk`) must be re-checked — if it regresses, normalize
   both index and query instead (e.g. fold diacritics in Rust before
   insert/query via a small helper), and update `kyoku-spec.md` §Search
   accordingly.
2. In `queries::search_tracks`, re-evaluate the query builder against
   trigram semantics: trigram matches substrings natively, so the `"term"*`
   prefix wrapping can become a plain quoted phrase `"term"` (prefix `*`
   is a no-op/warn under trigram — check the SQLite docs and keep whichever
   form tests confirm). Multi-term AND behavior must stay.
3. Tests (the user-level tests that were missing — this is the point):
   insert `靴の花火` / `ヨルシカ` / `花火大会` rows and assert
   `search_tracks("花火")` finds `靴の花火` and `花火大会`,
   `search_tracks("ルシカ")` finds `ヨルシカ`, `search_tracks("花")`
   finds both titles, Latin substring (`"love"` → "Love Song") still
   works, and `bjork` → `Björk` behavior is preserved (whichever way 1
   resolved it).
4. Update the two wrong claims in `kyoku-spec.md` (the unicode61
   "individual tokens" note and the `### Search` section) to describe the
   trigram design.

### L-42 — FTS drift repair only handles the empty case
In `src/tui/mod.rs::run`, replace `fts_count == 0 && track_count > 0` with
a divergence check: rebuild when `fts_count != track_count`. (Counts can
diverge legitimately only through corruption/manual edits; rebuild is
idempotent and cheap relative to startup.)

### L-48 — FTS update trigger fires on every track UPDATE
In the same v9 migration (it already recreates triggers if you choose to
restate them there — otherwise add `migrations/009_fts_trigger_of.sql`):
recreate `tracks_fts_update` as
`AFTER UPDATE OF title, artist, album_id ON tracks` with the same body.
Test: `update_track_path` must not churn FTS (assert via
`SELECT COUNT(*) FROM tracks_fts` + a search hit before/after a path
update), and renaming a title still re-syncs.

### L-31 — Global search post-filter drops album-title track hits
In `src/tui/views/global_search.rs::execute`, the FTS track hits are
re-filtered with `fuzzy::matches_any(query, &[&t.title, &artist])`, which
drops matches that hit via `album_title`. Add `album_title` to
`TrackRow` (extend the `map_track_row` SELECTs — they already LEFT JOIN
albums in some queries; make it uniform), then include it in the fuzzy
haystack. Test: seed an album 幻燈 with a track whose title shares no
terms; global-search `幻燈` must list the track.

### SYS-3d — `search_collections` LIKE escaping
In `queries::search_collections`, route the pattern through the existing
`escape_like` helper and add `ESCAPE '\\'` to the LIKE clause, mirroring
`search_albums`. Test: a collection literally named `100%` is found by
query `100%` and a query of `%` does not match everything.

---

## WP-B · Deletion & orphan safety — DEL-1, DEL-2, SEC-1, DB-6, ORG-5, DB-7

One coherent sweep: every flow that removes rows/files goes through the
same "who owns the survivor" rule. Do DEL-1 and SEC-1 first (data loss).

### DEL-1 — Batch collection delete destroys shared tracks (HIGH)
In `src/tui/views/collections.rs`, `InputMode::ConfirmBatchDelete` apply
arm: plans are computed once up front, then applied.
1. Restructure to per-collection plan→apply:
   ```rust
   for id in &ids {
       let plan = organizer::plan_delete_collection_with_roots(conn, music_dir, *id, &file_delete_roots)?;
       let result = organizer::apply_delete_collection_with_roots(conn, music_dir, &plan, delete_files, &file_delete_roots)?;
       ...
   }
   ```
   (Keep the *summary popup* computed from up-front plans — that's fine
   for display; just don't reuse those plans for the applies.) Note in a
   comment why: a later collection's "other home" may have been deleted
   by an earlier apply in the same batch.
2. Regression test (encode the review probe): track with no album,
   organized copies in collections A and B, batch-delete A+B with
   delete_files — assert the track row survives pointing at an existing
   file (whichever copy the implementation decides to keep), and at least
   one copy file survives. The correct end state: one file + one row;
   the current end state is zero files + dangling row.

### SEC-1 — Orphan unlink outside managed roots (HIGH)
In `src/core/organizer.rs`:
1. `apply_organize`'s file-orphan loop: skip the `std::fs::remove_file`
   (and keep the tracking row) unless
   `paths::path_is_strictly_inside(&entry.path, cleanup_roots)` — pass the
   roots in (the function already receives `cleanup_roots`; reuse them,
   and consider narrowing to `file_delete_roots` semantics — decide and
   document which root set orphan cleanup should honor).
2. Mirror the classification in `plan_organize` so the preview shows
   outside-root orphans as "kept (outside managed dirs)" instead of
   "will be deleted" — see `organize_preview::build_details` /
   `build_summary` and the `FileOrphanDetail` path.
3. Tests: orphan row at an absolute inbox path survives apply (row kept,
   file kept); orphan inside music_dir is still swept.

### DEL-2 — Row deleted despite failed file delete; worker orphans live rows
1. `src/core/pruner.rs::apply_delete_plan` and
   `organizer.rs::apply_delete_collection_with_roots`: when
   `remove_file` fails, do **not** delete that track's DB row; push into
   `errors` and continue. Where a row must go anyway (album delete flow),
   record the survivor via `queries::insert_orphan` so organize sweeps it
   deterministically instead of the library-audit lottery.
2. `src/tui/views/import/worker.rs` replace flow: move the
   `insert_orphan` call inside the `else` of the `delete_track` success
   branch (it currently runs even on failure).
3. Tests: chmod-0 a file (or equivalent) to force `remove_file` failure;
   assert the track row survives / the orphan row is written. Worker:
   inject a `delete_track` failure (e.g. FK-off harness or a stubbed
   connection wrapper if you introduce one) and assert no orphan row.

### DB-6 — Worker dup-replace not DB-atomic
In `worker.rs::run_import_worker`, wrap each group's DB mutations
(delete + orphan + insert + `update_track_mb` + album upsert +
collection membership) in `conn.unchecked_transaction()`. Tag writes
(`tagger::write_tags`) stay **outside** the tx, after commit — file I/O
must not hold the write lock. On tx failure: log the group, increment
`errors`, continue with the next group. Test: force an insert failure
mid-group (e.g. pre-seed a conflicting `file_path` via a second
connection) and assert the original track row is intact.

### ORG-5 — Prune leaks collection copies / re-import bait
1. In `apply_organize`'s missing-source prune loop: before `delete_track`,
   load the track's `collection_file_path`s (helper exists:
   `queries::get_tracks_delete_info`), delete the in-root copies (respect
   `cleanup_roots`), or `insert_orphan` them when deletion isn't wanted —
   pick delete-in-root to match prune semantics, document the choice.
2. Same for `pruner::apply_delete_plan` rows-only mode
   (`delete_files == false`): the copies are currently left referenced by
   nothing; record them as orphans (do **not** delete — the user declined
   file deletion).
3. Tests: pruned track with an organized collection copy leaves no file
   behind (or an orphan row, per choice); rows-only delete produces
   orphan rows for copies; `scan_inbox_with_report` no longer resurfaces
   either as unimported.

### DB-7 — v8 dedupe drops divergent collection-copy paths
In `src/db/schema.rs::dedupe_collections_nocase`: before deleting the
loser's memberships, reconcile rows where both sides have a
`collection_file_path`: prefer the non-NULL one on conflict
(`UPDATE ... SET collection_file_path = ? WHERE collection_id = ?keep
AND track_id = ? AND collection_file_path IS NULL`), and when both are
non-NULL and differ, `tracing::warn!` the loser path (or insert into
`orphaned_files` — the table exists now; the v8 migration predates its
use here). Only then `DELETE FROM collection_tracks WHERE
collection_id = ?loser`. Test: 'Mix'/'mix' sharing a track with
different copy paths; assert the surviving row has a path and the other
path is preserved/logged.

---

## WP-C · Organize races & cover moves — ORG-4, ORG-6, L-40

### ORG-4 — Prune-vs-move identity swap (HIGH)
In `src/core/organizer.rs::apply_organize`:
1. Move the missing-source prune loop **before** the moves loop (keep the
   apply-time `if path.exists() { continue; }` re-check — that still
   protects the remount/restore case; what changes is that no move has
   landed yet, so a planned destination can't masquerade as a restored
   source and the UNIQUE collision disappears).
2. Regression test (encode the review probe): track A row whose file is
   gone at path P; track B (same rendered target) imported at inbox path
   Q; plan + apply; assert B's move succeeds *including its DB update*,
   A's row is pruned, and P's content is B's. Assert zero errors.
3. While touching the planner: consider keeping missing-source paths in
   `used_paths` (defense in depth against any future code path that
   plans onto them) — only if it doesn't break the legit
   re-import-to-same-path flow; say what you decided in the commit.

### ORG-6 — Cover-move overwrite
1. `plan_organize`'s cover-move section: skip planning a cover move when
   `dest.exists()` and the existing file is not the same file as the
   source (reuse the `same_file` helper), or disambiguate the cover name
   through the same `disambiguate` closure the audio paths use (decide:
   overwriting a *stale kyoku-stamped* cover is fine, overwriting an
   untracked user file is not — simplest correct rule is skip + log).
2. `apply_organize`'s cover loop: add the same `cm.to.exists()` backstop
   the copies have.
3. Test: pre-existing untracked `cover.jpg` at the destination survives;
   a kyoku-tracked cover still follows its album.

### L-40 — `stamp_sibling_cover` re-stamps on every import
In `src/core/importer.rs::import` and
`src/tui/views/import/worker.rs::stamp_sibling_cover`: only stamp when
the album row was just created (`created == true` from
`get_or_create_album`) or when `cover_art_path` is currently NULL. Test:
import album from dir A with cover, re-import from dir B with a
different cover, assert the recorded path is still A's.

---

## WP-D · Import pipeline — IMP-1, WIZ-1, EXT-6, L-32, L-38, L-44, L-46, L-47

### IMP-1 — Feature-heavy albums import loose; unify the consistency twins
1. Extract one shared function (suggest `src/core/importer.rs`, used by
   both the CLI and `ImportGroup::has_consistent_album_tags`) that
   decides "does this group describe one album". Port the TUI extras
   (various-artists marker list, diversity heuristic) into it so CLI and
   TUI agree; normalize case/whitespace identically (trim + lowercase —
   currently only the CLI twin does).
2. Fix the diversity rule itself: collapse feature suffixes before
   counting — split each artist string on ` feat. ` / ` feat ` /
   ` ft. ` / ` featuring ` / ` with ` / ` x ` (case-insensitive, also
   Japanese ` feat.` variants if trivial) and count *primary* artists
   only; drop the absolute `>= 4` branch, keep the percentage rule
   (e.g. ≥40% of tracks with a differing primary, min 4 tracks).
3. Tests (encode the probe): 25-track album, 3 `feat.` guests →
   consistent (album created); genuine Various Artists comp →
   inconsistent (loose). Run the same cases through both the CLI
   `group_has_consistent_album_tags` and the TUI `ImportGroup` method —
   one behavior.

### WIZ-1 — Summary dead-end on persistent fetch failure
In `src/tui/views/import/wizard.rs` / `render.rs`:
1. Track per-group full-release fetch failures (e.g.
   `full_release_failures: u8` on `ImportGroup`, incremented in the
   release_fetch drain when `msg.release` is `None`).
2. `ensure_summary_mb_fetches`: skip groups whose failure count ≥ 2 and
   mark them with a visible state (reuse `MbMatchState::Failed` or a new
   "MB data unavailable" note in the summary render).
3. The summary Enter gate (`pending_full_mb_fetch_count`) then lets the
   import proceed; the worker already tolerates empty tracklists
   (`worker.rs` mb_fetch_failed path). Test: simulate two failed fetches
   (inject a channel result with `release: None`), assert Enter reaches
   `start_import`.

### EXT-6 — Alias fetch `?` kills `fetch_release`
In `src/external/musicbrainz.rs::fetch_release`: replace both
`self.get_artist_aliases(...)?` sites (release artist + per-track loop)
with a logged default, e.g.
```rust
let aliases = self.get_artist_aliases(mbid, &raw_artist)
    .unwrap_or_else(|e| { tracing::debug!("alias lookup failed for {mbid}: {e}"); &[] });
```
(returning `&[]` needs the cache-backed signature adjusted — simplest is
to make `get_artist_aliases` itself return an empty slice on error and
log once). Test: with a client whose artist-alias endpoint fails (see
TEST-5's mock), `fetch_release` still returns the release with canonical
names.

### L-32 — Worker swallows `insert_track` errors
In `worker.rs`: change `Err(_) => errors += 1` to log path + error like
the neighboring sites (`tracing::warn!("insert_track({}) failed: {e}",
path_str)`), and include the failing path in the completion summary if
trivial to thread through. No behavior change otherwise.

### L-38 — Multi-disc filename parsing
In `worker.rs::parse_filename_position`: first try `^(\d{1,2})[-._](\d{1,3})`
(disc-track); if it matches, return `(disc, track)` and have
`match_group_to_mb` pass 1 use it for both slot parts; fall back to the
current leading-digits behavior otherwise. Tests: `2-01 - Song.flac` →
disc 2 track 1; `05. Foo.flac` → track 5 (unchanged).

### L-44 — Manual-MBID result clobbers a later Skip
In `render.rs` mbid_fetch drain: before applying the result, check
`group.user_decided`; if set (user moved on / pressed S), discard the
result (or apply candidates without touching `action`/
`selected_candidate`). Test: mark group decided, deliver a manual-fetch
result, assert action unchanged.

### L-46 — CLI import: multi-album folder
In `src/core/importer.rs`: within a source-directory group, sub-group by
normalized (album, album_artist) before the consistency check — each
coherent sub-group becomes its own album; only the truly uncoherent
remainder goes loose. Tests: one dir with Album A + Album B → two albums;
mixed bag dir → loose.

### L-47 — CLI import transaction scope
In `src/core/importer.rs::import`: move `get_or_create_collection` and
the `track_exists_by_path` duplicate check inside the main transaction;
on per-track `insert_track` UNIQUE failure, count as duplicate and
continue rather than aborting the batch. Tests: import with one
pre-existing path → batch completes, one duplicate counted.

---

## WP-E · Filesystem & network hardening — SEC-2, SEC-3, SEC-4, SEC-5, SEC-6

### SEC-2 — Template escape (HIGH)
1. `src/core/template.rs`: add
   `validate_rendered_path(rendered: &Path) -> Result<()>` rejecting
   absolute paths and any `Component::RootDir | ParentDir | Prefix`;
   call it from the three target builders in
   `organizer.rs::TrackTargets::{primary_target, collection_target}` and
   fail the plan with a clear error naming the template.
2. `Settings::load`: validate the four configured templates by rendering
   them against a dummy `TemplateVars` and applying the same check;
   `kyoku setup` should refuse to save a failing template. Warn, don't
   silently rewrite.
3. Tests: templates `/tmp/{title}.flac` and `../x/{title}.flac` are
   rejected at plan time; metadata values with `/`, `..`, `..\\..` still
   sanitize fine (existing sanitize tests keep passing).

### SEC-3 — Symlink-aware destructive checks
In `src/core/paths.rs`: add `path_is_strictly_inside_canonical(path,
roots)` that canonicalizes the target's parent and each root (existence
permitting) before the prefix check; use it in
`pruner::apply_delete_plan`, `organizer::apply_delete_collection_with_roots`,
and the orphan-unlink gate from SEC-1. Document that import-time
canonicalization already narrows the hole; this closes the
post-import-symlink case. Test (unix): symlink inside music_dir pointing
outside → destructive ops refuse; normal in-root deletes still pass.

### SEC-4 — Predictable tmp names
1. `src/core/tagger.rs::tmp_path_for` / `write_tags`: create the tmp with
   `tempfile::NamedTempFile::new_in(parent)` (or
   `OpenOptions::create_new(true)` + random suffix), keep the
   `.kyoku-tmp` marker in the name so the scanner keeps skipping it
   (check `importer::scan_audio_files_with_report`'s `TMP_MARKER`
   `contains` match still holds), and `persist()`/rename after verifying
   `file_type().is_file()`.
2. Same for `detail.rs::save_fetched_cover`.
3. Sweep stale `.kyoku-tmp*` siblings older than a day on
   `write_tags` entry (best-effort, log only). Tests: pre-created
   read-only file at the old deterministic name no longer matters;
   successful write leaves no tmp behind.

### SEC-5 — Cover download hardening
In `src/external/cover_art_archive.rs::attempt` and
`detail.rs::save_fetched_cover`:
1. Enforce a max size (suggest 32 MiB): read with
   `resp.take(MAX + 1)` and error when exceeded; validate Content-Type
   against `image/(jpeg|png|webp)` before writing.
2. `save_fetched_cover`: when `dest.exists()` and the DB doesn't already
   point at that exact path, route through the same overwrite-confirm
   path (return a distinct error the caller maps to the confirm popup —
   see `start_cover_fetch`'s existing gate).
3. Tests (with TEST-5's mock): oversize body errors; `text/html` 200
   rejected; untracked existing dest triggers the confirm branch.

### SEC-6 — MBID validation
Validate manual MBIDs as UUIDs in `wizard.rs::fetch_mbid` (regex or the
`uuid` crate — check whether adding a dep is acceptable; a hand-rolled
8-4-4-4-12 hex check is fine) and percent-encode path segments in
`musicbrainz.rs` URL builders (a `urlencoding` helper already exists —
reuse it for path segments). Tests: `fetch_mbid("not-a-uuid")` shows an
input error without hitting the network.

---

## WP-F · Unicode, matching & small TUI fixes — L-36, L-37, L-33, L-34, L-35, L-43

### L-36 — Fullwidth Latin / Vietnamese script classification
In `src/external/matching.rs::scripts_of`: map `0xFF01..=0xFF5E` and
`0x1E00..=0x1EFF` to `S_LATIN`. Consider NFKC-folding both sides before
`sim()` (fullwidth `ＨＡＮＡＢＩＥ` vs `HANABIE` then scores ≈ 1) — a
tiny local fold helper is fine, no new dep needed for the common ranges.
Tests: `is_pure_latin("ＨＡＮＡＢＩＥ")` (post-NFKC) true; score for
fullwidth vs halfwidth same-name artist ≥ 0.9.

### L-37 — Title factor uses pairing, not zip
In `matching.rs::score_release`: replace the positional `zip` with the
same greedy best-match used by `worker::match_group_to_mb` (extract the
similarity pass into a shared helper so worker and scorer can't drift —
SYS-3 again). Tests: partial album (local 5–11 of 11) scores the title
factor high against the right release.

### L-33 — Template substitution atomicity
In `template.rs::render_path`: rewrite as a single left-to-right scan —
on `{name[:fmt]}`, substitute the (sanitized) value; text outside
placeholders is copied verbatim; substituted values are never re-scanned.
Keep behavior identical for well-formed templates (the existing test
suite must pass unchanged). Test: artist `Foo {track}` renders literally.

### L-34 — `{year:4}` padding
In `template.rs::format_number`: zero-pad all-numeric width specs
(`{:0>width$}`), keep `0` as plain. Update the doc comment. Test:
`{year:4}` on 97 → `0097`.

### L-35 — Control chars in text inputs
In `src/tui/widgets/input.rs::handle_key`: ignore `Char(c)` when
`key.modifiers` contains `CONTROL` or `ALT`. Check the editor flow
afterwards: Ctrl+S while editing a field must reach `is_save` (the
editor's `handle_key` checks editing first — make the edit-mode branch
forward Ctrl+S to save instead of swallowing it). Tests: synthesize
`KeyEvent` with CONTROL modifier, value unchanged.

### L-43 — CAA comment drift
One-line: align the `fetch_front` fallback comment with the code (or
make a fixed-size 404 authoritative as documented — either, say which).

---

## WP-G · Consistency unification (SYS-3) — SYS-3a, SYS-3b

Depends on WP-A (SYS-3d) and WP-D (IMP-1's twin) landing; this package
finishes the pattern. The v9/v10 migration from WP-A is the vehicle.

### SYS-3a — Album identity case sensitivity
1. `queries::get_or_create_album`: match `title COLLATE NOCASE AND
   album_artist COLLATE NOCASE IS ?2` (verify `IS` + `COLLATE` interplay
   in a test — if awkward, use `(album_artist IS NULL AND ?2 IS NULL) OR
   album_artist = ?2 COLLATE NOCASE`). Keep first-seen spelling for
   display (do not overwrite the stored title on a case-variant hit).
2. Migration (v10): merge existing case-variant duplicate albums —
   repoint `tracks.album_id` to the lowest id per (lower(title),
   lower(album_artist)) group, delete the losers. Procedural Rust, same
   shape as `dedupe_collections_nocase`. Then a UNIQUE index on the
   lowered pair (expression index) to keep it closed.
3. Tests: "Album A" then "album a" imports → one album row; organize
   renders one directory.

### SYS-3b — Collection names: Unicode-aware uniqueness
1. Add `name_key TEXT` to `collections` (v10 migration): fill with
   NFKC-ish + lowercase key (Rust side; a small fold helper — full NFKC
   needs no dep if you limit to casefold + fullwidth→halfwidth), UNIQUE
   index on `name_key`.
2. `get_or_create_collection` and rename paths look up by `name_key`.
3. Tests: "CAFÉ"/"café" → one collection (the review probe); ASCII
   behavior unchanged; migration merges the probe's duplicates.

---

## WP-H · Performance — PERF-1, L-41

### PERF-1 — UI-thread syscall storms
1. `list_all_known_paths` / `refresh_counts`: cache the known-paths set
   (e.g. behind a `OnceCell<Mutex<…>>` keyed by a dirty flag bumped on
   every mutating query helper), and make canonicalization lazy — compare
   scanner output against both the stored absolute form and
   `canonicalize(scanned)` (one syscall per *scanned* file, not per
   *known* path). Measure a 10k-row refresh before/after and note numbers
   in the commit.
2. Global search: add the same 150 ms debounce the local filter has
   (`app.rs` `search_debounce` pattern), drop the per-call
   `SELECT COUNT(*) FROM tracks_fts` gate (cache "fts usable" at startup
   + after rebuild — L-42 makes that reliable), and move the three
   queries into one `execute` that short-circuits on empty query.
3. `get_collection_tracks`: push ordering/pagination into SQL — order by
   effective position is currently computed in Rust
   (`collection_order::ordered_indices`); persist effective positions on
   mutation (they're already computed in
   `get_collection_effective_positions`) and then
   `ORDER BY position LIMIT ? OFFSET ?`. If that's too big for this WP,
   leave the sort in Rust but at least avoid re-running it per page —
   note the choice in the commit.
4. Organize plan/apply: run on a background thread with a progress
   channel like the import worker (only if it fits the WP; otherwise
   file a follow-up note in the review doc).

### L-41 — View caps
Add SQL pagination to `list_albums`/`list_loose_tracks`/
`get_collection_tracks` (interacts with PERF-1 #3 — coordinate), and a
"+N more" indicator row when truncated. Test: seed 501 albums, page
through, indicator shows.

---

## WP-I · Tests & toolchain — TEST-2, TEST-3, TEST-4, TEST-5

### TEST-2 — fmt/toolchain
Pick one: (a) adopt current stable — run `cargo +stable fmt` repo-wide,
commit the diff, pin CI `cargo fmt --check` to the same stable; or
(b) pin everything to 1.94.1 (release workflow included) and document
`cargo +1.94.1 fmt` in the README/dev docs. (a) is recommended — edition
2024 idioms are already in use. Update `justfile fmt-check` comment.

### TEST-3 — Binary depends on the library
`src/main.rs`: delete the `mod cli; mod config; …` block, `use
kyoku::{cli::{Cli, Command}, config, core, db, external, tui};` (keep
`error` internal paths as needed). Confirm `cargo test` runs the 214
unit tests once. Watch for `pub(crate)` items the bin used — make them
`pub` in the lib or move the logic.

### TEST-4 — TMPDIR brittleness
In `justfile`: `test: TMPDIR="${TMPDIR:-/tmp}" cargo test` plus a
pre-check `[ -d "$TMPDIR" ] || mkdir -p "$TMPDIR"`; or add a tiny
`tests/common` helper that creates the temp root. Document in
`tests/fixtures/README.md`.

### TEST-5 — Mock HTTP for MB/CAA
Introduce a base-URL parameter on `MbClient`/`CaaClient` (defaulting to
the real hosts) and spin a tiny `std::net::TcpListener` mock in tests
(no new dep needed for fixed responses). Cover: MB 500-then-200 retry,
MB 400 no-retry with error body surfaced, alias-endpoint failure →
`fetch_release` still succeeds (EXT-6's test), CAA original-404 →
1200 → 500 chain, fixed-size 404 → `Ok(None)`, content-type → extension
mapping, oversize body → error (SEC-5's test).

---

## Suggested sequencing

1. **WP-C** first — smallest, highest-certainty data-loss fixes
   (ORG-4 is a loop reorder + test; ORG-6/L-40 are localized).
2. **WP-B** — the rest of the data-loss/integrity sweep (DEL-1, SEC-1,
   DEL-2, DB-6, ORG-5, DB-7). Lands the "who owns the survivor" rule
   once, in one reviewable diff.
3. **WP-A** — FTS-1 is the flagship user-facing fix; it carries the v9
   migration that WP-G's v10 builds on.
4. **WP-D** — import correctness; IMP-1's shared consistency function
   should land before WP-G's unification pass.
5. **WP-E** — hardening; independent of the above, can run parallel.
6. **WP-G** — finishes SYS-3; needs WP-A's migration scaffolding.
7. **WP-F**, **WP-H**, **WP-I** in any order (WP-I's mock-HTTP infra
   makes WP-E's SEC-5 tests easier — do TEST-5 before SEC-5 if you want
   the network tests).
