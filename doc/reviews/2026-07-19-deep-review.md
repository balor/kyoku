# Deep codebase review — 2026-07-19

Second full pass at commit `551067e` (~26.5k lines of Rust), following the
gpt-5.5 review (`review/correctness.md`, `security.md`,
`quality-tests.md` — since removed from the tree)
and the June tracker ([2026-06-12-codebase-review.md](2026-06-12-codebase-review.md)).
Baseline at review time: build clean, 214 tests passing (run twice — see
TEST-3), 5 clippy warnings, `cargo fmt --check` failing (TEST-2).

Method: every `src/*.rs` file read in full, all 7 migrations, spec
cross-check, plus **empirical probes** — throwaway integration tests run
against the real code to confirm four findings end-to-end (marked
"verified by probe"; probes removed afterwards).

This is a **living tracking document**: after each fix lands, update the
finding's `Status` (and add the commit hash). Statuses: `open`,
`in-progress`, `fixed (<commit>)`, `wontfix (<reason>)`, `obsolete`.

Finding IDs are stable — reference them in commit messages. IDs continue
the June series where applicable (ORG-*, EXT-*, DB-*, SEC-*, TEST-*);
new series: FTS-*, DEL-*, IMP-*, WIZ-*, PERF-*. Low-severity items
continue the L-* numbering from June (last was L-30).

**Step-by-step fix instructions for every open finding live in
[2026-07-19-fix-instructions.md](2026-07-19-fix-instructions.md)**,
grouped into self-contained work packages (WP-A … WP-I) with suggested
sequencing.

Disposition of prior reviews: June findings marked fixed check out as
actually fixed. The gpt-5.5 findings (`review/correctness.md`,
`security.md`, `quality-tests.md` — since removed from the tree) are all
still open and all confirmed real; they are folded into this tracker as
SEC-*/DB-6/ORG-6/DB-7/TEST-* so there is one index again.

---

## Verdict

Well-built codebase — the organize planner's collision model and the June
fixes are genuinely careful. But beneath the already-reported surface:
**the flagship search feature is broken for CJK** (the spec's tokenizer
assumption is factually wrong), **two data-loss paths hide in cross-plan
interactions** (batch collection delete; missing-source prune racing a
move), and the June review's still-open **SYS-3 (inconsistent twins)**
turns out to have four concrete, user-visible instances.

---

## High

### FTS-1 — CJK substring search silently fails; spec's tokenizer assumption is wrong
**Status:** open · **Confidence:** certain (verified by probe)
The spec states twice that "FTS5 unicode61 tokenizer handles CJK
characters as individual tokens (each character = one token)"
(`kyoku-spec.md:228-229`, `:916`). False: unicode61 glues each contiguous
CJK run into *one* token, and `search_tracks` only issues prefix queries
(`"term"*`, `src/db/queries.rs:517-527`). Measured against the bundled
SQLite: `花火` does not match `靴の花火`, `ルシカ` does not match
`ヨルシカ`, `花` does not match `靴の花火`. Only token-*prefix* hits work
(which is why the spec's example `初音` → `初音ミク` passes and nobody
noticed). The LIKE fallback does real substring matching but only runs
when `tracks_fts` is completely empty (`queries.rs:505-515`) — never in
practice. Compounded by masking: local view filters use
`fuzzy::matches_any`, which *does* handle CJK substrings, so the feature
appears to work in dogfooding. Why tests missed it: the entire
`search_tracks` test surface is two tests — one for quote handling, one
that forces the fallback with `DELETE FROM tracks_fts`; zero CJK in FTS
mode. Fix: migrate to `tokenize='trigram'` in a v9 schema step (true
substring semantics for CJK *and* Latin), or union FTS with LIKE for
non-ASCII queries.

### DEL-1 — Batch collection delete destroys tracks whose "other home" is in the same batch
**Status:** open · **Confidence:** certain (verified by probe)
Batch delete computes all per-collection plans up front against pre-batch
DB state (`src/tui/views/collections.rs`, `InputMode::ConfirmBatchDelete`
arm), then applies them sequentially. Track T, no album, primary file =
its copy in collection A, second copy in collection B, batch-delete A+B
with "delete files": plan A promotes `T.file_path` to B's copy and
deletes A's copy; apply B then deletes B's copy per its stale plan.
Probe end state: **both files deleted, `T.file_path` dangling** → next
organize prunes the row. The confirmation popup reports "0 track(s) will
be removed" because each plan sees the *other* collection as T's home.
Single-collection delete is safe; the bug is purely that plans are not
re-evaluated between applies. Fix: re-plan each collection after the
previous apply, or plan against the union of in-batch collections.

### ORG-4 — Missing-source prune races move destinations: silent file-identity swap
**Status:** open · **Confidence:** certain (verified by probe)
`apply_organize` runs moves before the missing-source prune
(`src/core/organizer.rs`). Scenario: track A's file deleted by hand;
track B (same album metadata) freshly imported to the inbox; organize.
B's rendered target equals A's old path (A's path was removed from
`used_paths` at plan time along with every filtered track). The move
lands; `update_track_path(B)` fails on `UNIQUE constraint failed:
tracks.file_path` (A's row still holds it); the prune loop's apply-time
re-check (`if path.exists() { continue; }`) sees the path now exists —
it's B's file — and keeps A. End state (probe): A's row points at B's
audio, B's row points at its now-empty inbox path, one error line. Next
organize completes the swap silently: A "exists" and organizes, B is
pruned. Fix: run the missing-source prune **before** the moves (the
apply-time `exists()` re-check still protects the remount case), or keep
missing-source paths claimed in `used_paths` and treat
move-onto-missing-path as an implicit prune.

### SEC-1 — Orphaned_files cleanup can delete files outside managed roots *(from security.md)*
**Status:** open · **Confidence:** certain
`apply_organize` unlinks every `plan.file_orphans` entry with zero root
checks (`src/core/organizer.rs`, orphan loop). Live path confirmed:
worker replace flow orphans `repl.file_path`
(`src/tui/views/import/worker.rs`), which is any track found by
`find_track_by_mbid` / `find_track_by_album_slot` — including tracks at
absolute inbox paths outside `music_dir`. Next organize deletes that
outside file without prompting. Fix: gate orphan unlink on
`path_is_strictly_inside` against an explicit deletion root set at both
plan and apply time.

### SEC-2 — Organize templates can escape `music_dir` *(from correctness.md/security.md)*
**Status:** open · **Confidence:** certain
`render_path` sanitizes substituted values but not literal template text
(`src/core/template.rs:28-51`); all three target builders do
`music_dir.join(render_path(...))` unchecked; `Settings::load` performs
no template validation. A template of `/tmp/{title}.flac` ignores
`music_dir`; `../outside/{title}.flac` escapes it, and escaped paths can
later pass the lexical `starts_with` managed-root checks. Fix: validate
rendered paths (reject `RootDir`/`ParentDir`/prefix components) at plan
time and validate configured templates at load/setup time.

---

## Medium

### IMP-1 — Feature-heavy albums misclassified as compilations, imported loose (TUI only)
**Status:** open · **Confidence:** certain (verified by probe)
`ImportGroup::has_consistent_album_tags`
(`src/tui/views/import.rs:100-145`) counts *raw distinct artist-tag
strings*; `unique_artists.len() >= 4` fires regardless of album size.
Probe: 25-track album, consistent album+album_artist, 3 `feat.` guests
(4 distinct strings) → "compilation" → `group_is_real_album` false → 25
tracks import loose, no album. Mainstream hip-hop/pop albums hit this
routinely. Also a SYS-3 instance: the CLI twin
(`src/core/importer.rs::group_has_consistent_album_tags`) has **no**
diversity rule and different normalization (trim+lowercase vs exact), so
the same folder imports as an album via `kyoku import` but loose via the
TUI. Fix: collapse `feat.`/`ft.`/`with` suffixes before counting, scale
the rule by album size, unify both twins into one shared function.

### WIZ-1 — Import wizard dead-ends on the summary when a full-release fetch persistently fails
**Status:** open · **Confidence:** certain (code-traced)
Summary Enter fires `ensure_summary_mb_fetches` and returns while any
fetch is pending (`src/tui/views/import/wizard.rs`). On failure the drain
(`src/tui/views/import/render.rs`) clears `full_release_fetching` but
leaves `release.tracks` empty, so the next Enter re-fires the same fetch
and returns again — forever. With MB down or a dead MBID, the wizard
cannot import *anything* (even unrelated as-is groups) until the user
guesses they must `p`-back and demote the offending group. Fix: give up
after N failures per group, mark it "MB metadata unavailable", and let
the import proceed (the worker already tolerates missing tracklists).

### EXT-6 — Optional per-artist alias fetch kills the whole `fetch_release`
**Status:** open · **Confidence:** certain (code-traced)
`MbClient::fetch_release` propagates errors from the Latin-preference
alias lookups with `?` (`src/external/musicbrainz.rs:455-520`). The alias
lists are pure enrichment — canonical names are already in hand. One
transient `/artist/{mbid}?inc=aliases` failure (post-retry) fails the
entire release fetch: MBID dup detection silently disables for that
group, and the worker falls back to "import without MB metadata". Fix:
default to empty alias lists (log at debug); the resolver already falls
back to canonical.

### ORG-5 — Missing-source prune leaks collection copies; rows-only deletes breed re-import bait
**Status:** open · **Confidence:** certain (code-traced)
The prune loop calls only `delete_track` (cascades `collection_tracks`);
organized collection copies stay on disk referenced by nothing and no
`orphaned_files` entry. The next import wizard library-audit
(`scan_inbox_with_report` over `music_dir`) resurfaces them as
*unimported new files* — the pruned track comes back as a "new" find.
Same shape in `pruner::apply_delete_plan` and
`apply_delete_collection_with_roots` when deleting rows only. Fix: on
prune/row-delete, delete in-root copies or `insert_orphan` them so
organize sweeps deterministically.

### DEL-2 — DB row deleted despite failed file delete; worker orphans a *live* row's file
**Status:** open · **Confidence:** certain (code-traced)
`pruner::apply_delete_plan` and `apply_delete_collection_with_roots`
record a failed `remove_file` in `errors` but delete the track row
anyway — the survivor file is referenced by nothing (the
`orphaned_files` mechanism exists for exactly this but is only used by
the dup-replace flow). Sub-case: the worker's replace flow
(`worker.rs:243-268`) runs `insert_orphan` even when `delete_track` just
failed (only the counter is behind `else`) — orphaning the file of a
still-live row, which the next organize then deletes from under it.
Fix: skip the row delete (or `insert_orphan` the survivor) on file-delete
failure; gate the worker's `insert_orphan` on `delete_track` success.

### DB-6 — TUI duplicate-replace import is not DB-atomic *(from correctness.md/quality-tests.md)*
**Status:** open · **Confidence:** certain
The worker performs replace as separate statements on an autocommit
connection: `delete_track` → `insert_orphan` → (later) `insert_track` →
`update_track_mb` → collection membership (`worker.rs:240-376`). A
mid-sequence failure leaves the old row gone and the replacement partial.
Fix: wrap each group's DB mutations in a transaction; tag writes after
commit.

### ORG-6 — Cover-art organize can overwrite an existing destination cover *(from correctness.md)*
**Status:** open · **Confidence:** certain
Cover moves target a fixed `<album_dir>/cover.<ext>` with no
disambiguation and no `dest.exists()` guard at apply
(`organizer.rs:552-569`, `:768-790`); `rename`/`fs::copy` clobber
silently. Compounds with L-40 (re-stamping on every re-import). Fix:
reserve/disambiguate cover destinations at plan time or skip/error on an
existing different file at apply time.

### SEC-3 — Symlink traversal defeats lexical managed-root checks *(from security.md, narrowed)*
**Status:** open · **Confidence:** certain (mechanism), narrowed exposure
Destructive root checks are lexical (`src/core/paths.rs:46-55`); scans
follow symlinks. However, imports canonicalize paths and `to_db_path` has
a canonical fallback, so normally-stored paths resolve through real
locations — the residual hole is a symlink introduced *after* import
inside `music_dir`, or pre-planted symlinked dirs in an untrusted inbox.
Fix: for destructive ops, canonicalize root and target parent and require
containment; consider making `follow_links` configurable.

### SEC-4 — Predictable temp-file names allow symlink/clobber races *(from security.md)*
**Status:** open · **Confidence:** certain (mechanism)
Tag writes use a deterministic sibling tmp (`song.kyoku-tmp.mp3`,
`src/core/tagger.rs:516-521`); cover save uses `cover.<ext>.kyoku-tmp`
(`detail.rs:623-628`). Both follow symlinks at the tmp path. Fix:
create-new random temp names in the same dir (`tempfile::NamedTempFile::
new_in` or `OpenOptions::create_new(true)`), verify regular file before
rename. Related: stale `.kyoku-tmp` files are never swept (L-39).

### SEC-5 — Cover download overwrites untracked local covers; no size cap or MIME validation *(from security.md)*
**Status:** open · **Confidence:** certain
The overwrite confirm keys off `album.cover_art_path` only
(`detail.rs:500-507`); an untracked `cover.jpg` in the album dir is
silently replaced (`detail.rs:599-631`). CAA bodies are read fully into
memory with no cap (`cover_art_archive.rs:155-168`), and any 200
Content-Type is saved (an HTML error page becomes `cover.jpg`). Fix:
`dest.exists()` check in `save_fetched_cover`, max byte cap via
`.take(max+1)`, whitelist image MIMEs.

### DB-7 — v8 collection de-dupe can drop a collection-copy path *(from correctness.md)*
**Status:** open · **Confidence:** likely
`dedupe_collections_nocase` merges memberships with `INSERT OR IGNORE`
then deletes the loser's rows (`src/db/schema.rs:176-187`); a losing row
with a distinct `collection_file_path` leaves that on-disk copy
untracked. Fix: reconcile conflicting paths before delete (prefer
non-NULL; log/preserve divergent ones).

### PERF-1 — O(library) syscall storms on the TUI's UI thread
**Status:** open · **Confidence:** certain (code-traced)
(a) `refresh_counts` → `scan_inbox` → `list_all_known_paths`
canonicalizes *every* known path (one syscall each) after every
delete/organize/import completion — 10k tracks ≈ 10k+ stats per refresh.
(b) Global search runs 3 query sets per keystroke with **no debounce**,
including `SELECT COUNT(*) FROM tracks_fts` (full FTS scan) per call and
a full `list_collections` join/aggregate. (c) `plan_organize`/`apply_organize`
run synchronously in key handlers (exists() per track, per-track
membership queries, file I/O). (d) `get_collection_tracks` loads and
sorts the whole collection to serve a 500-row page (LIMIT applied after
the full in-memory sort). Fix: cache the known-paths set with
invalidation on writes; debounce global search and drop the COUNT(*)
gate; push pagination into SQL; background the organize apply.

### TEST-2 — `cargo fmt --check` fails under current stable; toolchain pins drift *(from quality-tests.md)*
**Status:** open · **Confidence:** certain (reproduced)
Repo pins Rust 1.94.1 (`.mise.toml`, `flake.nix`); release workflow uses
unpinned stable; rustfmt 1.9.0 disagrees with the checked-in style, so
`just check` fails for anyone on current stable. Fix: adopt current
stable rustfmt and commit the reformat (or pin CI to 1.94.1 and document
`cargo +1.94.1 fmt`).

### TEST-3 — Unit tests run twice: binary re-declares the module tree *(from quality-tests.md)*
**Status:** open · **Confidence:** certain (reproduced: 214 × 2)
`src/main.rs` re-declares every `mod` instead of `use kyoku::…`, doubling
test time and compiling two module trees that can drift. Fix: make the
binary depend on the library crate.

### TEST-4 — Tests brittle when TMPDIR points at a missing dir *(from quality-tests.md)*
**Status:** open · **Confidence:** certain (reproduced)
72 failures with a stale `TMPDIR`; all pass with `TMPDIR=/tmp`. Fix:
normalize/create the temp root in `just test` or a test helper.

### TEST-5 — External HTTP retry/status behavior unvalidated *(from quality-tests.md)*
**Status:** open · **Confidence:** certain
MB `attempt_get`/`get_json_body` and CAA `fetch_front`/`attempt` have no
mock-server tests (404 vs 5xx, retry-once, fallback chains). Fix: inject
base URL or spin a local mock; cover the listed cases.

---

## Low

### SYS-3a — Album identity is case-sensitive while grouping is case-insensitive
**Status:** open
`get_or_create_album` matches `title = ?1 AND album_artist IS ?2`
exactly; grouping/matching normalize case. "Album A" vs "album a" → two
album rows → two case-differing directories that collide on
case-insensitive filesystems. Fix: match `COLLATE NOCASE`, keep
first-seen spelling for display.

### SYS-3b — Collection name uniqueness: ASCII-only at runtime, Unicode-aware in the migration
**Status:** open · verified by probe
Runtime lookup/index use `COLLATE NOCASE` (SQLite folds ASCII only) —
`"CAFÉ"` and `"café"` coexist (probe: 2 rows). The v8 migration's dedupe
uses Rust `to_lowercase()` and *would* merge them. Runtime re-creates
what the migration removed. Fix: normalize (NFKC + casefold) names into a
`name_key` column with a plain UNIQUE index.

### SYS-3d — `search_collections` doesn't escape LIKE wildcards
**Status:** open
`queries.rs:640-660` — `%`/`_` in the filter act as wildcards;
`search_albums` escapes properly. Fix: share `escape_like`.

### L-31 — Global search fuzzy post-filter drops FTS album-title hits
**Status:** open
FTS matches tracks on `album_title`; `GlobalSearchView::execute` then
requires every term to substring-match *track title or artist*, so
"tracks on album 幻燈" never appear as track results. The LIKE
fallback's album branch is dead code for the same reason. Fix: include
the album title in the fuzzy haystack (and return it in `TrackRow`), or
drop the post-filter.

### L-32 — Worker swallows `insert_track` errors entirely
**Status:** open
`Err(_) => errors += 1` in `worker.rs` — no log, no path, no reason; a
UNIQUE violation becomes "Errors: 1" with zero diagnostics. Fix: log like
every other failure site in the worker.

### L-33 — Template placeholder injection via metadata values
**Status:** open
`render_path` substitutes variables sequentially into the whole template;
`sanitize_path_component` keeps `{`/`}`, so a tag value containing
`{track}` gets expanded by the later numeric pass. Can't escape dirs
(values are sanitized) but renders garbage paths. Fix: single-pass
substitution; never re-scan replacement text.

### L-34 — `{year:4}` space-pads instead of zero-padding
**Status:** open
`format_number` maps non-`0`-prefixed widths to `{:>width$}` (spaces).
`{year:4}` on year 97 → `"Album (  97)"`; doc comment promises 4-digit
year. Fix: zero-pad numeric specs (or document the padding).

### L-35 — Text inputs insert control-modified characters
**Status:** open
`TextInput::handle_key` matches `KeyCode::Char(c)` without checking
modifiers: Ctrl+C types `c`, Ctrl+S types `s` — so in the tag editor,
Ctrl+S *while editing a field* inserts "s" instead of saving (the status
bar advertises Ctrl+S at all times). Fix: ignore CONTROL/ALT-modified
chars.

### L-36 — `scripts_of` misses fullwidth Latin (and Latin Ext. Additional)
**Status:** open
Fullwidth romaji (`ＨＡＮＡＢＩＥ`, U+FF21–FF5A) gets no script bit →
`scripts_differ` false → Jaro-Winkler fullwidth-vs-halfwidth ≈ 0 → artist
factor tanks on identical names. Vietnamese (0x1E00–0x1EFF) also
unclassified. Fix: map `0xFF01..=0xFF5E` and `0x1E00..=0x1EFF` to
`S_LATIN`; consider NFKC-normalizing before scoring.

### L-37 — Match scoring compares track titles positionally, contradicting the careful pairing
**Status:** open
`score_release` zips local titles with MB tracks index-wise, while the
worker pairs via `match_group_to_mb` (number → similarity → positional).
The exact case the pairing exists for (partial album, tracks 5–11 of 11)
scores titles 5↔1, 6↔2 ≈ 0 and drags candidates down. Fix: reuse
`match_group_to_mb` (or best-match on the same similarity) for the title
factor.

### L-38 — `parse_filename_position` mangles multi-disc naming
**Status:** open
`2-01 - Song.flac` parses leading digits as position **2**; disc-2-track-1
pairs to MB position 2 of disc 1. Fix: parse `D-TT`/`D.TT` conventions
into `(disc, position)`.

### L-39 — Tag writes lose xattrs/ACLs, convert symlinks to regular files, leave stale tmps
**Status:** open
copy-tmp-rename preserves permissions but not xattrs/ACLs; on a symlinked
track, `fs::copy` follows the link and `rename` replaces the *link* with
a regular file. Crash-left `.kyoku-tmp` siblings are scanner-skipped but
never swept. Fix: refuse/special-case symlink targets; copy xattrs where
feasible; sweep stale tmps on startup or import scan.

### L-40 — `stamp_sibling_cover` re-stamps on every import
**Status:** open
CLI and worker both overwrite `albums.cover_art_path` whenever the latest
source dir has a cover — a re-import from another folder silently swaps
the recorded cover (and the next organize's cover-move clobbers the old
one — see ORG-6). Fix: stamp only when unset or on album creation.

### L-41 — Hard caps silently truncate views
**Status:** open
Albums 500 (`library.rs` load), collection tracks 500 (applied *after* a
full in-memory sort), loose tracks 5000 (`detail.rs`), search results 500.
No pagination, no "and N more". Fix: SQL-level pagination + a truncation
indicator.

### L-42 — FTS drift repair only handles the fully-empty case
**Status:** open
Startup rebuilds FTS only when `fts_count == 0 && track_count > 0`
(`src/tui/mod.rs:39-45`); a partially drifted index is never repaired and
search silently prefers it. Fix: rebuild when counts diverge (cheap
`COUNT(*)` comparison at startup only).

### L-43 — CAA `Original` chain contradicts its own comment
**Status:** open
Comment says fixed-size 404 is authoritative; code falls through 1200 →
500 on any 404 in the chain (`cover_art_archive.rs:62-112`). Harmless
(one wasted request) — align comment or code.

### L-44 — Manual-MBID result clobbers a later Skip decision
**Status:** open
The MBID fetch is keyed by group index; pressing `S` on that group while
the fetch is in flight gets overwritten when the result lands (sets
AcceptMb + `user_decided`). Fix: skip applying a stale manual-fetch
result when `user_decided` is set.

### L-45 — `update_track_fields` silently ignores non-whitelisted fields
**Status:** open
Unknown fields `continue` instead of erroring — a caller bug looks like
success. Fix: return a `Config`/usage error.

### L-46 — CLI import of a multi-album folder imports everything loose
**Status:** open (design gap)
`album_group_key` = source dir; one mixed group with inconsistent tags →
no albums. Fix: sub-group by normalized (album, album_artist) within a
directory in the CLI path.

### L-47 — CLI `import` does collection-create and the duplicate check outside the transaction
**Status:** open
Main-tx failure leaves a created collection behind; a concurrent insert
between check and tx aborts the whole batch instead of skipping one file.
Fix: move both inside the tx; handle UNIQUE per-track.

### L-48 — FTS update trigger fires on every track UPDATE
**Status:** open
Moves rewrite `file_path`, status flips, `modified_date` touches — all
pay a delete+insert into FTS (migrations/002). Fix: `AFTER UPDATE OF
title, artist, album_id ON tracks`.

### SEC-6 — MBID URL path segments unencoded/unvalidated *(from security.md)*
**Status:** open
Manual MBID input takes `rsplit('/')` and inserts the result into URL
paths unencoded (`wizard.rs::fetch_mbid`, `musicbrainz.rs:509-514`).
Fixed host, so no SSRF — just confusing failures. Fix: validate as UUID
before fetch; percent-encode path segments.

---

## Systemic update

### SYS-3 — Inconsistent twins (June, still open) — now with four concrete instances
**Status:** open · **Confidence:** certain
The June review named the pattern; this pass found the live instances:
(a) album identity case sensitivity (SYS-3a), (b) collection-name Unicode
asymmetry (SYS-3b), (c) CLI-vs-TUI compilation/consistency rules (folded
into IMP-1), (d) LIKE escaping (SYS-3d). Also showing in triplicate: the
three near-identical organize-apply loops in `src/tui/app/handlers.rs`
(notice text and counter sets already drifted). Unify, don't patch.

---

## Positive architecture notes

- Organize planning: reservation pre-pass (row-order independence),
  slot-free/disambiguate with on-disk + DB occupancy, apply-time
  backstops, occupied-vs-orphan canonical guards, missing-volume prune
  guard — the strongest part of the codebase.
- Dup detection: conservative defaults, skip-before-replace fail-safe
  ordering, pass suppression, multi-disc `taken[]` tracking.
- Dead-thread detection on every background channel; mutex poison
  recovery; shared throttled MB client honoring the 1 req/s policy.
- Path handling: relative storage + canonical fallback + NFC/NFD
  awareness, well-tested relocation story.
- Test suite: real e2e, real fixture audio, regression tests that explain
  the bug they pin.

## Verification appendix

Probe-verified this pass (throwaway integration tests, since removed):
FTS-1 (live FTS5 behavior table), DEL-1 (end-to-end file+DB state),
ORG-4 (end-to-end identity swap), IMP-1 (3 guests → loose), SYS-3b
(2 rows from "CAFÉ"/"café"), FTS5 punctuation-query safety (no error —
*not* a bug). Code-traced findings say so; none rely on speculation.
`cargo test` 214 pass; `cargo clippy` 5 pre-existing warnings; `cargo
fmt --check` fails (TEST-2).
