//! Import-time duplicate detection.
//!
//! Runs after the review-summary step and before the worker fires. Scans
//! every track in every non-skipped group against:
//!
//!   * **Other tracks in the same batch** — catches the "user dropped the
//!     same album in twice" shape.
//!   * **Existing tracks in the library** — catches re-import of content
//!     that's already in the DB.
//!
//! Detection runs three passes per invocation:
//!   1. **Album-slot**: `(album_id, disc, position)` — fast, DB-only, and
//!      catches the "partial then full re-import" case.
//!   2. **Recording MBID**: for AcceptMb groups whose selected candidate
//!      has a fetched tracklist, the MB `recording_id` at the local
//!      track's position is matched against `tracks.mbid` in the library
//!      and against other batch tracks. Skipped for (group, track) pairs
//!      that album-slot already flagged — MBID only *disambiguates*, it
//!      never replaces the slot signal.
//!   3. **Album + title**: intra-batch only. Keyed on
//!      `(album_key, normalized_title)` where `album_key` is a set of
//!      best-effort stable identifiers for the group's album (DB
//!      `album_id` if it exists, MB `release.id` for AcceptMb, and a
//!      normalized tag `album|album_artist` fallback). Catches the case
//!      where the same release is imported twice from different sources
//!      and the two copies disagree on disc/track numbering — the
//!      album-slot pass misses those, but the titles still line up.
//!
//! The MBID pass depends on `ImportView::ensure_full_release_for_group`
//! having populated `MbCandidate.release.tracks` via a background fetch.
//! If the fetch hasn't landed yet, MBID detection is a no-op for that
//! group — the user just won't see MBID conflicts until the next tick.
//!
//! Conflicts are returned, not resolved — the wizard renders a picker and
//! collects a `ConflictDecision` per conflict, which the worker then
//! applies.
//!
//! Out of scope:
//!   * Fuzzy title/duration matching for as-is imports.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::db::queries::{self, ExistingTrackRef};
use crate::error::Result;

use super::{GroupAction, ImportGroup};

/// Pointer to a specific track inside the import batch, addressed by its
/// group index + in-group index. Stable across detection and resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchTrackRef {
    pub group: usize,
    pub index: usize,
}

/// The "other" side of a conflict. The "new" side is always a batch track
/// (something we're about to import).
#[derive(Debug, Clone)]
pub enum DupOther {
    /// Already in the library — DB row, on-disk file.
    Library(ExistingTrackRef),
    /// Another track in the same import batch.
    Batch(BatchTrackRef),
}

/// Why we think two tracks are duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DupSignal {
    /// Same `(album_id, disc, track_number)` slot.
    AlbumSlot,
    /// Same MusicBrainz `recording_id`. Derived from the selected
    /// MB candidate's tracklist at the local track's position; only
    /// raised for `AcceptMb` groups whose full release has been
    /// fetched (`ensure_full_release_for_group`).
    Mbid,
    /// Same album + same (normalized) track title. Used as a fallback
    /// when two batch groups describe the same release with disagreeing
    /// disc/position tags — album-slot would miss them but titles still
    /// line up. Intra-batch only; library-side title matching is too
    /// noisy (remixes, alternate versions) to raise automatically.
    AlbumTitle,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub new: BatchTrackRef,
    pub other: DupOther,
    /// Why we think this is a duplicate. Drives the resolver's header
    /// text so the user understands whether they're looking at a
    /// same-slot or same-MBID conflict.
    pub signal: DupSignal,
}

/// What the user chose for a conflict. Default (before they choose) is
/// `KeepOther`, which is the conservative "don't change anything" pick:
/// library rows stay, earlier batch tracks stay, new/later sides drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    /// Keep the new (incoming) side; if other is Library, mark it
    /// orphaned + delete the DB row. If other is Batch, drop it from the
    /// import.
    KeepNew,
    /// Keep the other side; the new side is dropped from the import.
    KeepOther,
}

/// Intent for what will happen to a specific batch track once the worker
/// runs. Computed from the decisions vector; the worker reads it per
/// track to decide whether to insert, skip, or replace.
#[derive(Debug, Clone, Default)]
pub struct BatchTrackPlan {
    /// If true, worker must not insert this track.
    pub skip: bool,
    /// If present, delete this existing library track (and orphan its
    /// file) before inserting the batch track.
    pub replace_existing: Option<ReplaceExisting>,
}

#[derive(Debug, Clone)]
pub struct ReplaceExisting {
    pub id: i64,
    /// The rest of these fields are snapshotted purely so the worker can
    /// populate the `orphaned_files` row without re-reading the track
    /// from the DB after deleting it. They *look* unused at the type-sig
    /// level but aren't — see `worker::run_import_worker`.
    pub file_path: String,
    pub title: String,
    pub artist: Option<String>,
    pub album_title: Option<String>,
}

/// Compute per-track plans given the original groups and user decisions.
/// Returns a `Vec<Vec<BatchTrackPlan>>` keyed by (group_idx, track_idx).
pub fn plan_from_decisions(
    groups: &[ImportGroup],
    conflicts: &[Conflict],
    decisions: &[ConflictDecision],
) -> Vec<Vec<BatchTrackPlan>> {
    let mut plans: Vec<Vec<BatchTrackPlan>> = groups
        .iter()
        .map(|g| vec![BatchTrackPlan::default(); g.tracks.len()])
        .collect();

    for (c, d) in conflicts.iter().zip(decisions.iter()) {
        match d {
            ConflictDecision::KeepNew => match &c.other {
                DupOther::Library(existing) => {
                    plans[c.new.group][c.new.index].replace_existing = Some(ReplaceExisting {
                        id: existing.id,
                        file_path: existing.file_path.clone(),
                        title: existing.title.clone(),
                        artist: existing.artist.clone(),
                        album_title: existing.album_title.clone(),
                    });
                }
                DupOther::Batch(other) => {
                    plans[other.group][other.index].skip = true;
                }
            },
            ConflictDecision::KeepOther => {
                plans[c.new.group][c.new.index].skip = true;
            }
        }
    }

    plans
}

/// Resolve the album identity each group would commit to, so we can match
/// it against existing rows. Mirrors the worker's own resolution: MB album
/// for AcceptMb (looked up by `(mb_title, mb_artist)`), as-is album for
/// AcceptAsIs with a "real album" signature, None otherwise.
fn target_album_id_for_group(conn: &Connection, group: &ImportGroup) -> Result<Option<i64>> {
    match group.action {
        GroupAction::AcceptMb => {
            let Some(idx) = group.selected_candidate else {
                return Ok(None);
            };
            let Some(cand) = group.mb_candidates.get(idx) else {
                return Ok(None);
            };
            lookup_album(conn, &cand.release.title, Some(&cand.release.artist))
        }
        GroupAction::AcceptAsIs => {
            // Only resolve an album_id when the group's tags actually
            // describe a single coherent album. Heterogeneous groups
            // (mixed singles, hand-picked bundles, "Various" comps) get
            // committed by the worker as loose tracks with no album,
            // so they have no shared slot space to dedupe against —
            // pretending they do (by picking the first track's album)
            // produces phantom conflicts every time the first track's
            // album happens to overlap with something in the library.
            if !group.has_consistent_album_tags() {
                return Ok(None);
            }
            let Some((_, Some(td))) = group.tracks.first() else {
                return Ok(None);
            };
            let Some(album) = td.album.as_deref() else {
                return Ok(None);
            };
            let album_artist = td.album_artist.as_deref().or(td.artist.as_deref());
            lookup_album(conn, album, album_artist)
        }
        GroupAction::Loose | GroupAction::Skip => Ok(None),
    }
}

/// Compute a set of best-effort stable identifiers for the album a
/// group would commit to. Used by the title-based intra-batch pass so
/// two groups describing the same release match even when the album
/// isn't in the DB yet and even when the two groups arrived via
/// different actions (one AcceptMb, one AcceptAsIs).
///
/// Prefixes keep the namespaces separate so "db:12" can't accidentally
/// collide with "mb:12".
fn album_keys_for_group(conn: &Connection, group: &ImportGroup) -> Vec<String> {
    let mut keys = Vec::new();
    if let Ok(Some(id)) = target_album_id_for_group(conn, group) {
        keys.push(format!("db:{}", id));
    }
    if group.action == GroupAction::AcceptMb
        && let Some(idx) = group.selected_candidate
        && let Some(cand) = group.mb_candidates.get(idx)
    {
        if !cand.release.id.is_empty() {
            keys.push(format!("mb:{}", cand.release.id));
        }
        // Also emit a tag-equivalent key so this AcceptMb group
        // matches a sibling AcceptAsIs group of the same release.
        keys.push(format!(
            "tag:{}|{}",
            normalize_album_token(&cand.release.title),
            normalize_album_token(&cand.release.artist),
        ));
    }
    // Only emit a tag-based album key for AcceptAsIs groups whose tags
    // actually describe one album. A heterogeneous group's first-track
    // album is a misleading proxy and would let unrelated singles share
    // an album_key (and thus collide in MBID / album+title passes).
    if group.action == GroupAction::AcceptAsIs
        && group.has_consistent_album_tags()
        && let Some((_, Some(td))) = group.tracks.first()
        && let Some(album) = td.album.as_deref()
    {
        let artist = td
            .album_artist
            .as_deref()
            .or(td.artist.as_deref())
            .unwrap_or("");
        keys.push(format!(
            "tag:{}|{}",
            normalize_album_token(album),
            normalize_album_token(artist),
        ));
    }
    keys
}

/// Album-identity tokens for a library track, in the same string format
/// as `album_keys_for_group`. Used by the MBID pass to decide whether a
/// library hit lives on the same album the user is importing into — same
/// recording on a different album is a legitimate re-release, not a dup.
fn album_keys_for_existing_track(t: &crate::db::queries::ExistingTrackRef) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(id) = t.album_id {
        keys.push(format!("db:{}", id));
    }
    if let (Some(album), Some(artist)) = (t.album_title.as_deref(), t.artist.as_deref()) {
        keys.push(format!(
            "tag:{}|{}",
            normalize_album_token(album),
            normalize_album_token(artist),
        ));
    }
    keys
}

/// Lowercase + whitespace-collapsed token. Used for both album keys and
/// track titles — a forgiving match, but not so aggressive that it
/// conflates genuinely different titles (no punctuation stripping, no
/// feature-artist removal).
fn normalize_album_token(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for ch in s.trim().chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            last_space = false;
        }
    }
    out
}

/// Normalized track title for the AlbumTitle pass. Empty titles return
/// `None` so the pass skips them — we don't want a group of
/// tag-less/title-less tracks to all collapse onto the empty key.
fn normalized_title(group: &ImportGroup, track_idx: usize) -> Option<String> {
    let (track, tag) = group.tracks.get(track_idx)?;
    // Prefer the raw tag title; fall back to the Track's title (which
    // the scanner may have already filled from the filename).
    let raw = tag
        .as_ref()
        .and_then(|t| t.title.as_deref())
        .unwrap_or(track.title.as_str());
    let norm = normalize_album_token(raw);
    if norm.is_empty() { None } else { Some(norm) }
}

/// Find an existing album row by title + album_artist. Returns None if
/// absent — detection then treats the group as "new album, nothing to
/// clash with in the library".
fn lookup_album(conn: &Connection, title: &str, album_artist: Option<&str>) -> Result<Option<i64>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM albums WHERE title = ?1 AND album_artist IS ?2",
            rusqlite::params![title, album_artist],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Detect duplicate conflicts for an about-to-run import.
///
/// Runs two passes: album-slot (DB-only), then recording MBID (needs the
/// group's MB release to have been fetched). The second pass only fires
/// for `(group, track)` pairs that the first pass left alone — MBID only
/// disambiguates, it never emits a second conflict for a pair that
/// album-slot already covered.
pub fn detect(
    conn: &Connection,
    music_dir: &std::path::Path,
    groups: &[ImportGroup],
) -> Result<Vec<Conflict>> {
    let mut conflicts = Vec::new();
    // Maps (album_id, disc, position) → first batch track that claimed it.
    // Used to catch intra-batch dupes.
    let mut batch_slots: HashMap<(i64, u32, u32), BatchTrackRef> = HashMap::new();
    // (group_idx, track_idx) pairs already flagged by the album-slot pass —
    // the MBID pass skips them.
    let mut flagged: HashSet<(usize, usize)> = HashSet::new();

    // Album identity tokens per group. Computed once and shared between
    // the MBID pass (pass 2) and the album+title pass (pass 3) — both
    // need to ask "do these two groups describe the same album?". The
    // MBID pass uses this to avoid flagging the same recording across
    // *different* albums (singles, best-ofs, special editions).
    let group_album_keys: Vec<Vec<String>> = groups
        .iter()
        .map(|g| {
            if matches!(g.action, GroupAction::Skip | GroupAction::Loose) {
                Vec::new()
            } else {
                album_keys_for_group(conn, g)
            }
        })
        .collect();
    let shares_key =
        |a: &[String], b: &[String]| -> bool { a.iter().any(|x| b.iter().any(|y| x == y)) };

    // ── Pass 1: album-slot ────────────────────────────────────────────
    for (gi, group) in groups.iter().enumerate() {
        if matches!(group.action, GroupAction::Skip | GroupAction::Loose) {
            continue;
        }
        let album_id = target_album_id_for_group(conn, group)?;
        let Some(album_id) = album_id else {
            continue;
        };

        for (ti, (track, _tag)) in group.tracks.iter().enumerate() {
            let Some(pos) = track.track_number else {
                continue;
            };
            if pos == 0 {
                continue;
            }
            let disc = track.disc_number;
            let key = (album_id, disc, pos);
            let new_ref = BatchTrackRef {
                group: gi,
                index: ti,
            };

            // Library dup?
            if let Some(existing) =
                queries::find_track_by_album_slot(conn, music_dir, album_id, disc, pos)?
            {
                conflicts.push(Conflict {
                    new: new_ref,
                    other: DupOther::Library(existing),
                    signal: DupSignal::AlbumSlot,
                });
                flagged.insert((gi, ti));
                // Still record in batch_slots so a *third* occurrence in
                // the same batch at this slot conflicts with this one
                // and not with the library (keeps the conflict list from
                // fanning out).
                batch_slots.entry(key).or_insert(new_ref);
                continue;
            }

            // Intra-batch dup?
            if let Some(earlier) = batch_slots.get(&key) {
                conflicts.push(Conflict {
                    new: new_ref,
                    other: DupOther::Batch(*earlier),
                    signal: DupSignal::AlbumSlot,
                });
                flagged.insert((gi, ti));
            } else {
                batch_slots.insert(key, new_ref);
            }
        }
    }

    // ── Pass 2: recording MBID ────────────────────────────────────────
    // Maps recording_id → first batch track that claimed it. Scoped to
    // this pass so album-slot hits don't accidentally suppress it.
    //
    // Recording IDs are precomputed per group using the *same* greedy
    // pairing the worker uses at import time (taken[]-tracked, so each
    // MB track can only be claimed once). Doing it per-track with
    // `iter().find(|t| t.position == pos)` was the old bug: on multi-disc
    // releases MbTrack positions aren't globally unique (MB reports
    // per-disc positions, flattened into release.tracks), so disc-2 pos=1
    // would spuriously share a recording_id with disc-1 pos=1 — producing
    // phantom intra-group MBID conflicts for every disc-2 track.
    // Gating on shared album_key is what keeps a recording that's
    // legitimately published on multiple releases (single → best-of →
    // special edition) from raising a conflict every time. The same
    // MBID across *different* albums is a re-release, not a duplicate.
    let group_rec_ids: Vec<Vec<Option<String>>> =
        groups.iter().map(recording_ids_for_group).collect();
    // Maps recording_id → list of (album_keys, BatchTrackRef) claims so
    // a later track only matches an earlier one when they actually share
    // an album.
    let mut batch_mbid_claims: HashMap<String, Vec<(Vec<String>, BatchTrackRef)>> = HashMap::new();
    for (gi, group) in groups.iter().enumerate() {
        if matches!(group.action, GroupAction::Skip | GroupAction::Loose) {
            continue;
        }
        let group_keys = &group_album_keys[gi];
        for (ti, _) in group.tracks.iter().enumerate() {
            if flagged.contains(&(gi, ti)) {
                continue;
            }
            let Some(recording_id) = group_rec_ids[gi].get(ti).cloned().flatten() else {
                continue;
            };
            let new_ref = BatchTrackRef {
                group: gi,
                index: ti,
            };

            // Library dup by MBID? Only counts when the existing track
            // belongs to the same album the user is importing into.
            if let Some(existing) = queries::find_track_by_mbid(conn, music_dir, &recording_id)? {
                let existing_keys = album_keys_for_existing_track(&existing);
                if shares_key(&existing_keys, group_keys) {
                    conflicts.push(Conflict {
                        new: new_ref,
                        other: DupOther::Library(existing),
                        signal: DupSignal::Mbid,
                    });
                    flagged.insert((gi, ti));
                    batch_mbid_claims
                        .entry(recording_id)
                        .or_default()
                        .push((group_keys.clone(), new_ref));
                    continue;
                }
            }

            // Intra-batch dup by MBID? Only when the two batch groups
            // describe the same album.
            let mut matched: Option<BatchTrackRef> = None;
            if let Some(claims) = batch_mbid_claims.get(&recording_id) {
                for (claim_keys, claim_ref) in claims {
                    if shares_key(claim_keys, group_keys) {
                        matched = Some(*claim_ref);
                        break;
                    }
                }
            }
            if let Some(earlier) = matched {
                conflicts.push(Conflict {
                    new: new_ref,
                    other: DupOther::Batch(earlier),
                    signal: DupSignal::Mbid,
                });
                flagged.insert((gi, ti));
            } else {
                batch_mbid_claims
                    .entry(recording_id)
                    .or_default()
                    .push((group_keys.clone(), new_ref));
            }
        }
    }

    // ── Pass 3: album + title (cross-group only) ──────────────────────
    // Catches two groups importing the same release from different
    // sources when their disc/position tags disagree (or when the album
    // isn't yet in the DB, so pass 1 was a no-op). For each group we
    // precompute a set of candidate album_keys; two tracks collide if
    // any album_key + normalized_title tuple is shared.
    //
    // Deliberately *cross-group only*: within a single group, a shared
    // album + shared normalized title almost always means legitimate
    // alt-versions (feat./remix/instrumental) where the differentiation
    // lives in the artist field or only in the filename — not a dup.
    // Pass 1 already handles genuine same-group same-slot dups.
    // Library-side title matching is deferred for the same reason
    // (remixes, alt-versions are too noisy to raise automatically).
    // Pool of claims from earlier groups. Each incoming track picks the
    // best still-unclaimed entry, preferring a position match when one
    // exists — so a release with several tracks sharing a title (e.g.
    // multiple "feat." versions) pairs correctly across groups by slot
    // number rather than all collapsing onto the first claim.
    //
    // Position match is only preferred, not required — the disc-divergent
    // re-import case has two copies of the same album with disagreeing
    // disc/position tags, so same-title tracks there find each other via
    // the title-only fallback.
    //
    // `taken[]` ensures each earlier-group claim pairs with at most one
    // later-group track; claims aren't reused.
    // One entry per earlier-group track; `album_keys` lists every key
    // the track's group emits (so a sibling group matches via any of
    // them). `taken` is per-entry so claiming via one key also prevents
    // matching via another key — a track can only pair with one later
    // counterpart.
    struct Claim {
        album_keys: Vec<String>,
        title: String,
        position: Option<u32>,
        track_ref: BatchTrackRef,
    }
    let mut pool: Vec<Claim> = Vec::new();
    let mut taken: Vec<bool> = Vec::new();

    for (gi, group) in groups.iter().enumerate() {
        if matches!(group.action, GroupAction::Skip | GroupAction::Loose) {
            continue;
        }
        let keys = &group_album_keys[gi];
        if keys.is_empty() {
            continue;
        }
        // Buffer this group's new claims and only merge into the pool
        // after the group is fully processed. That's what keeps intra-
        // group same-title tracks from matching each other — they never
        // see their own group's entries during lookup.
        let mut new_claims: Vec<Claim> = Vec::new();
        for (ti, (track, _)) in group.tracks.iter().enumerate() {
            if flagged.contains(&(gi, ti)) {
                continue;
            }
            let Some(title) = normalized_title(group, ti) else {
                continue;
            };
            let pos = track.track_number;
            let new_ref = BatchTrackRef {
                group: gi,
                index: ti,
            };

            // Tier 1: exact (album_key, title, position).
            let mut chosen: Option<usize> = None;
            if let Some(p) = pos {
                for (ci, c) in pool.iter().enumerate() {
                    if taken[ci] || c.title != title || c.position != Some(p) {
                        continue;
                    }
                    if shares_key(&c.album_keys, keys) {
                        chosen = Some(ci);
                        break;
                    }
                }
            }
            // Tier 2: title-only fallback for disc-divergent reimports.
            if chosen.is_none() {
                for (ci, c) in pool.iter().enumerate() {
                    if taken[ci] || c.title != title {
                        continue;
                    }
                    if shares_key(&c.album_keys, keys) {
                        chosen = Some(ci);
                        break;
                    }
                }
            }

            if let Some(ci) = chosen {
                conflicts.push(Conflict {
                    new: new_ref,
                    other: DupOther::Batch(pool[ci].track_ref),
                    signal: DupSignal::AlbumTitle,
                });
                flagged.insert((gi, ti));
                taken[ci] = true;
                continue;
            }

            // No earlier match — register this track as a claim for
            // later groups to pair against.
            new_claims.push(Claim {
                album_keys: keys.clone(),
                title,
                position: pos,
                track_ref: new_ref,
            });
        }
        for c in new_claims {
            pool.push(c);
            taken.push(false);
        }
    }

    Ok(conflicts)
}

/// Resolve the MB `recording_id` each local track in a group would get
/// at import time. Returns a vec parallel to `group.tracks`.
///
/// Uses the *same* pairing function the worker runs at commit time
/// (`worker::match_group_to_mb`) so detection and import agree on which
/// local track maps to which MB track. That function tracks claimed MB
/// indices with a `taken[]` array, which is essential for multi-disc
/// releases: MB's per-disc positions are flattened into `release.tracks`,
/// so positions 1..N appear once per disc. Matching each local track
/// independently via `.find(|t| t.position == pos)` would assign every
/// disc's pos=N the disc-1 recording_id.
///
/// Returns an all-`None` vec for groups that aren't AcceptMb, have no
/// selected candidate, or whose release hasn't been fetched yet
/// (`release.tracks` empty — this is the pre-fetch state that the full
/// release fetch populates).
fn recording_ids_for_group(group: &ImportGroup) -> Vec<Option<String>> {
    let empty = vec![None; group.tracks.len()];
    if group.action != GroupAction::AcceptMb {
        return empty;
    }
    let Some(cand_idx) = group.selected_candidate else {
        return empty;
    };
    let Some(cand) = group.mb_candidates.get(cand_idx) else {
        return empty;
    };
    if cand.release.tracks.is_empty() {
        return empty;
    }
    let pairing = super::worker::match_group_to_mb(&group.tracks, &cand.release.tracks);
    pairing
        .into_iter()
        .map(|mi| {
            let mt = cand.release.tracks.get(mi?)?;
            if mt.recording_id.is_empty() {
                None
            } else {
                Some(mt.recording_id.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tagger::TagData;
    use crate::db;
    use crate::db::models::{AudioFormat, TagStatus, Track};
    use crate::tui::views::import::{GroupAction, ImportGroup, MbMatchState};
    use std::path::PathBuf;

    fn track(title: &str, track_number: Option<u32>, path: &str) -> (Track, Option<TagData>) {
        let t = Track {
            id: None,
            album_id: None,
            title: title.to_string(),
            artist: Some("Artist".to_string()),
            track_number,
            disc_number: 1,
            duration_ms: None,
            mbid: None,
            file_path: PathBuf::from(path),
            file_format: AudioFormat::Flac,
            bitrate: None,
            sample_rate: None,
            tag_status: TagStatus::Unmatched,
            source_dir: None,
        };
        let td = TagData {
            title: Some(title.to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            year: None,
            track_number,
            disc_number: Some(1),
            genre: None,
            duration: None,
            mb_release_id: None,
        };
        (t, Some(td))
    }

    /// Like `track`, but with custom album / album_artist tags. Used to
    /// build heterogeneous groups where each track points at a different
    /// album (the "user is bundling singles into a collection" shape).
    fn track_with_album(
        title: &str,
        track_number: Option<u32>,
        path: &str,
        album: &str,
        album_artist: &str,
    ) -> (Track, Option<TagData>) {
        let t = Track {
            id: None,
            album_id: None,
            title: title.to_string(),
            artist: Some(album_artist.to_string()),
            track_number,
            disc_number: 1,
            duration_ms: None,
            mbid: None,
            file_path: PathBuf::from(path),
            file_format: AudioFormat::Flac,
            bitrate: None,
            sample_rate: None,
            tag_status: TagStatus::Unmatched,
            source_dir: None,
        };
        let td = TagData {
            title: Some(title.to_string()),
            artist: Some(album_artist.to_string()),
            album: Some(album.to_string()),
            album_artist: Some(album_artist.to_string()),
            year: None,
            track_number,
            disc_number: Some(1),
            genre: None,
            duration: None,
            mb_release_id: None,
        };
        (t, Some(td))
    }

    fn group(
        name: &str,
        action: GroupAction,
        tracks: Vec<(Track, Option<TagData>)>,
    ) -> ImportGroup {
        ImportGroup {
            name: name.to_string(),
            tracks,
            action,
            mb_candidates: Vec::new(),
            selected_candidate: None,
            mb_state: MbMatchState::NotStarted,
            target_collection: String::new(),
            full_release_fetching: false,
            user_decided: false,
        }
    }

    /// Insert a pre-existing track into the library at album "Album" /
    /// artist "Artist", position `pos`. Returns the inserted track id.
    fn seed_library_track(conn: &Connection, pos: u32, path: &str) -> i64 {
        let (album_id, _) =
            queries::get_or_create_album(conn, "Album", Some("Artist"), None, None, 11).unwrap();
        let track = Track {
            id: None,
            album_id: Some(album_id),
            title: format!("Existing {}", pos),
            artist: Some("Artist".to_string()),
            track_number: Some(pos),
            disc_number: 1,
            duration_ms: None,
            mbid: None,
            file_path: PathBuf::from(path),
            file_format: AudioFormat::Flac,
            bitrate: Some(900),
            sample_rate: None,
            tag_status: TagStatus::Matched,
            source_dir: None,
        };
        queries::insert_track(
            conn,
            std::path::Path::new(""),
            &track,
            Some(album_id),
            Some(10_000),
        )
        .unwrap()
    }

    #[test]
    fn detects_library_conflict_by_album_slot() {
        let conn = db::open_memory().unwrap();
        seed_library_track(&conn, 5, "/lib/05.flac");

        // Incoming batch has a track at position 5 on the same album.
        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![track("New Five", Some(5), "/in/05.flac")],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert_eq!(conflicts.len(), 1);
        match &conflicts[0].other {
            DupOther::Library(e) => assert_eq!(e.file_path, "/lib/05.flac"),
            _ => panic!("expected Library conflict"),
        }
    }

    #[test]
    fn heterogeneous_group_does_not_borrow_first_tracks_album() {
        // Regression: importing a hand-picked bundle of singles (each
        // track with its own album tag) into a collection used to flag
        // every track at pos N against whatever the first track's album
        // happened to resolve to in the library. The worker treats such
        // groups as loose tracks with no album, so dup detection has to
        // do the same — otherwise the slot keys are pure phantoms.
        let conn = db::open_memory().unwrap();
        let (kyougen_id, _) =
            queries::get_or_create_album(&conn, "Kyougen", Some("Ado"), None, None, 12).unwrap();
        // Existing library track: Readymade @ pos 1 of Kyougen.
        queries::insert_track(
            &conn,
            std::path::Path::new(""),
            &Track {
                id: None,
                album_id: Some(kyougen_id),
                title: "Readymade".to_string(),
                artist: Some("Ado".to_string()),
                track_number: Some(1),
                disc_number: 1,
                duration_ms: None,
                mbid: None,
                file_path: PathBuf::from("/lib/Ado/Kyougen/01 Readymade.flac"),
                file_format: AudioFormat::Flac,
                bitrate: None,
                sample_rate: None,
                tag_status: TagStatus::Matched,
                source_dir: None,
            },
            Some(kyougen_id),
            None,
        )
        .unwrap();

        // Import: heterogeneous bundle of Ado singles, each at pos 1 on
        // its own single's album. First track *happens to* be a Kyougen
        // copy (pre-bug: would steal the album_id and flag every other
        // pos=1 track as a Kyougen slot conflict).
        let mut g = group(
            "Headphone Test",
            GroupAction::AcceptAsIs,
            vec![
                track_with_album("Readymade", Some(1), "/in/01.flac", "Kyougen", "Ado"),
                track_with_album("Usseewa", Some(1), "/in/02.flac", "Usseewa", "Ado"),
                track_with_album("Odo", Some(1), "/in/03.flac", "Odo", "Ado"),
            ],
        );
        g.target_collection = "Headphone Test".to_string();
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        // Only the legitimate Readymade-vs-Readymade match should fire.
        // No phantom Usseewa/Odo conflicts against the Kyougen slot.
        assert_eq!(
            conflicts.len(),
            0,
            "heterogeneous group has no album slot space — got phantom \
             conflicts: {:?}",
            conflicts
        );
    }

    #[test]
    fn detects_intra_batch_conflict() {
        let conn = db::open_memory().unwrap();
        // Library already has the album record but no tracks at these
        // slots — intra-batch needs an existing album_id to key on.
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![
                track("A", Some(5), "/in/a.flac"),
                track("B", Some(5), "/in/b.flac"),
            ],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert!(matches!(conflicts[0].other, DupOther::Batch(_)));
    }

    #[test]
    fn no_conflict_when_album_is_new() {
        let conn = db::open_memory().unwrap();
        // No album row exists → nothing in the library to clash with,
        // and intra-batch dedup is keyed on album_id which we haven't
        // got, so batch clashes aren't raised either. That's acceptable:
        // a brand-new album being imported with two tracks at the same
        // slot is pathological but not a regression vs today.
        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![track("A", Some(5), "/in/a.flac")],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn skipped_and_loose_groups_ignored() {
        let conn = db::open_memory().unwrap();
        seed_library_track(&conn, 5, "/lib/05.flac");

        let mut g_skip = group(
            "Album",
            GroupAction::Skip,
            vec![track("X", Some(5), "/in/x.flac")],
        );
        g_skip.action = GroupAction::Skip;
        let mut g_loose = group(
            "Album",
            GroupAction::Loose,
            vec![track("Y", Some(5), "/in/y.flac")],
        );
        g_loose.action = GroupAction::Loose;
        let conflicts = detect(&conn, std::path::Path::new(""), &[g_skip, g_loose]).unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn zero_position_and_missing_position_ignored() {
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();
        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![
                track("A", None, "/in/a.flac"),
                track("B", Some(0), "/in/b.flac"),
            ],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn plan_from_decisions_keep_new_replaces_library() {
        let existing = ExistingTrackRef {
            id: 42,
            album_id: Some(1),
            title: "Old".to_string(),
            artist: None,
            album_title: None,
            track_number: Some(5),
            disc_number: 1,
            duration_ms: None,
            bitrate: None,
            file_format: "flac".to_string(),
            file_path: "/lib/05.flac".to_string(),
            file_size: None,
            mbid: None,
            tag_status: "matched".to_string(),
        };
        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![track("A", Some(5), "/in/a.flac")],
        );
        let conflicts = vec![Conflict {
            new: BatchTrackRef { group: 0, index: 0 },
            other: DupOther::Library(existing),
            signal: DupSignal::AlbumSlot,
        }];
        let plans = plan_from_decisions(&[g], &conflicts, &[ConflictDecision::KeepNew]);
        assert_eq!(plans[0][0].replace_existing.as_ref().unwrap().id, 42);
        assert!(!plans[0][0].skip);
    }

    #[test]
    fn plan_from_decisions_keep_other_skips_new() {
        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![track("A", Some(5), "/in/a.flac")],
        );
        let existing = ExistingTrackRef {
            id: 42,
            album_id: Some(1),
            title: "Old".to_string(),
            artist: None,
            album_title: None,
            track_number: Some(5),
            disc_number: 1,
            duration_ms: None,
            bitrate: None,
            file_format: "flac".to_string(),
            file_path: "/lib/05.flac".to_string(),
            file_size: None,
            mbid: None,
            tag_status: "matched".to_string(),
        };
        let conflicts = vec![Conflict {
            new: BatchTrackRef { group: 0, index: 0 },
            other: DupOther::Library(existing),
            signal: DupSignal::AlbumSlot,
        }];
        let plans = plan_from_decisions(&[g], &conflicts, &[ConflictDecision::KeepOther]);
        assert!(plans[0][0].skip);
        assert!(plans[0][0].replace_existing.is_none());
    }

    #[test]
    fn plan_from_decisions_intra_batch_keep_new_drops_other() {
        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![
                track("A", Some(5), "/in/a.flac"),
                track("B", Some(5), "/in/b.flac"),
            ],
        );
        let conflicts = vec![Conflict {
            new: BatchTrackRef { group: 0, index: 1 },
            other: DupOther::Batch(BatchTrackRef { group: 0, index: 0 }),
            signal: DupSignal::AlbumSlot,
        }];
        let plans = plan_from_decisions(&[g], &conflicts, &[ConflictDecision::KeepNew]);
        assert!(plans[0][0].skip, "earlier batch track dropped");
        assert!(!plans[0][1].skip, "newer batch track kept");
    }

    // ── MBID-pass helpers & tests ──────────────────────────────────────

    use crate::external::matching::MatchScore;
    use crate::external::musicbrainz::{MbRelease, MbTrack};
    use crate::tui::views::import::MbCandidate;

    /// Build an MB candidate whose release has a single track at `pos`
    /// with `recording_id`. Attaches to the group's AcceptMb selection.
    fn candidate_with_recording(pos: u32, recording_id: &str) -> MbCandidate {
        let release = MbRelease {
            id: "release-mbid".to_string(),
            title: "Album".to_string(),
            artist: "Artist".to_string(),
            year: None,
            country: None,
            label: None,
            status: None,
            track_count: 1,
            medium_count: 1,
            tracks: vec![MbTrack {
                disc: 1,
                position: pos,
                title: format!("Track {}", pos),
                artist: None,
                duration_ms: None,
                recording_id: recording_id.to_string(),
            }],
            api_score: 0,
            release_group_id: None,
            group_min_year: None,
        };
        MbCandidate {
            release,
            score: MatchScore {
                total: 0.0,
                artist: 0.0,
                album: 0.0,
                track_count: 0.0,
                year: 0.0,
                duration: 0.0,
                tracks: 0.0,
                country: 0.0,
                original: 0.0,
            },
        }
    }

    fn mb_group(
        name: &str,
        tracks: Vec<(Track, Option<TagData>)>,
        cand: MbCandidate,
    ) -> ImportGroup {
        ImportGroup {
            name: name.to_string(),
            tracks,
            action: GroupAction::AcceptMb,
            mb_candidates: vec![cand],
            selected_candidate: Some(0),
            mb_state: MbMatchState::Done,
            target_collection: String::new(),
            full_release_fetching: false,
            user_decided: false,
        }
    }

    /// Stamp an mbid onto an existing track row.
    fn set_track_mbid(conn: &Connection, track_id: i64, mbid: &str) {
        conn.execute(
            "UPDATE tracks SET mbid = ?1 WHERE id = ?2",
            rusqlite::params![mbid, track_id],
        )
        .unwrap();
    }

    #[test]
    fn mbid_pass_ignores_recording_on_different_album() {
        // The "best-of / special edition" case: a recording legitimately
        // appears on more than one release. Same MBID across different
        // albums is a re-release, not a duplicate.
        let conn = db::open_memory().unwrap();
        let (album_id, _) =
            queries::get_or_create_album(&conn, "Different Album", Some("Artist"), None, None, 1)
                .unwrap();
        let existing_id = queries::insert_track(
            &conn,
            std::path::Path::new(""),
            &Track {
                id: None,
                album_id: Some(album_id),
                title: "Existing".to_string(),
                artist: Some("Artist".to_string()),
                track_number: Some(3),
                disc_number: 1,
                duration_ms: None,
                mbid: None,
                file_path: PathBuf::from("/lib/old.flac"),
                file_format: AudioFormat::Flac,
                bitrate: None,
                sample_rate: None,
                tag_status: TagStatus::Matched,
                source_dir: None,
            },
            Some(album_id),
            None,
        )
        .unwrap();
        set_track_mbid(&conn, existing_id, "rec-abc");

        // Incoming: same recording_id, different album.
        let g = mb_group(
            "New Album",
            vec![track("New", Some(5), "/in/05.flac")],
            candidate_with_recording(5, "rec-abc"),
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert!(
            conflicts.is_empty(),
            "same MBID on a different album must not raise a conflict, got {:?}",
            conflicts
        );
    }

    #[test]
    fn mbid_pass_flags_same_recording_within_same_album_at_diff_position() {
        // Same album, same recording — but the album-slot pass misses it
        // because the local track number disagrees with the library row.
        // This is the case where the MBID pass legitimately fires.
        let conn = db::open_memory().unwrap();
        let (album_id, _) =
            queries::get_or_create_album(&conn, "Shared Album", Some("Artist"), None, None, 1)
                .unwrap();
        let existing_id = queries::insert_track(
            &conn,
            std::path::Path::new(""),
            &Track {
                id: None,
                album_id: Some(album_id),
                title: "Existing".to_string(),
                artist: Some("Artist".to_string()),
                track_number: Some(3),
                disc_number: 1,
                duration_ms: None,
                mbid: None,
                file_path: PathBuf::from("/lib/old.flac"),
                file_format: AudioFormat::Flac,
                bitrate: None,
                sample_rate: None,
                tag_status: TagStatus::Matched,
                source_dir: None,
            },
            Some(album_id),
            None,
        )
        .unwrap();
        set_track_mbid(&conn, existing_id, "rec-abc");

        // Incoming: AcceptAsIs group whose tag-derived album_key matches
        // the existing album ("shared album|artist"), MB candidate puts
        // rec-abc at pos 5 — different from the library's pos 3, so
        // album-slot won't flag it; MBID will.
        let g = mb_group(
            "Shared Album",
            vec![track("New", Some(5), "/in/05.flac")],
            candidate_with_recording(5, "rec-abc"),
        );
        // mb_group sets AcceptMb with the candidate's release.title as
        // the tag-derived album token — patch it to "Shared Album".
        let mut g = g;
        g.mb_candidates[0].release.title = "Shared Album".to_string();
        g.mb_candidates[0].release.artist = "Artist".to_string();
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].signal, DupSignal::Mbid);
        match &conflicts[0].other {
            DupOther::Library(e) => assert_eq!(e.file_path, "/lib/old.flac"),
            _ => panic!("expected Library conflict"),
        }
    }

    #[test]
    fn detects_intra_batch_conflict_by_mbid() {
        let conn = db::open_memory().unwrap();
        // Two AcceptMb groups that happen to share a recording MBID at
        // the same local position — same song, two different source
        // dirs dropped into one import.
        let g1 = mb_group(
            "G1",
            vec![track("A", Some(1), "/in/g1/01.flac")],
            candidate_with_recording(1, "rec-shared"),
        );
        let g2 = mb_group(
            "G2",
            vec![track("A", Some(1), "/in/g2/01.flac")],
            candidate_with_recording(1, "rec-shared"),
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].signal, DupSignal::Mbid);
        match &conflicts[0].other {
            DupOther::Batch(earlier) => {
                assert_eq!(earlier.group, 0);
                assert_eq!(conflicts[0].new.group, 1);
            }
            _ => panic!("expected Batch conflict"),
        }
    }

    #[test]
    fn album_slot_hit_suppresses_mbid_pass_for_same_pair() {
        // Both signals would fire for the same (group, track); only
        // album-slot should make it into the output.
        let conn = db::open_memory().unwrap();
        let existing_id = seed_library_track(&conn, 5, "/lib/05.flac");
        set_track_mbid(&conn, existing_id, "rec-match");

        let g = mb_group(
            "Album",
            vec![track("New", Some(5), "/in/05.flac")],
            candidate_with_recording(5, "rec-match"),
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert_eq!(conflicts.len(), 1, "one conflict, not two");
        assert_eq!(conflicts[0].signal, DupSignal::AlbumSlot);
    }

    #[test]
    fn unfetched_release_means_no_mbid_pass() {
        // AcceptMb with a candidate, but tracks vec is empty (search
        // result not yet promoted to full release). MBID pass must be
        // a no-op — no HTTP happens, no phantom conflict.
        let conn = db::open_memory().unwrap();
        // Seed a library row with an MBID we'd otherwise clash on.
        let (album_id, _) =
            queries::get_or_create_album(&conn, "Other", Some("Artist"), None, None, 1).unwrap();
        let existing_id = queries::insert_track(
            &conn,
            std::path::Path::new(""),
            &Track {
                id: None,
                album_id: Some(album_id),
                title: "X".to_string(),
                artist: Some("Artist".to_string()),
                track_number: Some(1),
                disc_number: 1,
                duration_ms: None,
                mbid: None,
                file_path: PathBuf::from("/lib/x.flac"),
                file_format: AudioFormat::Flac,
                bitrate: None,
                sample_rate: None,
                tag_status: TagStatus::Matched,
                source_dir: None,
            },
            Some(album_id),
            None,
        )
        .unwrap();
        set_track_mbid(&conn, existing_id, "rec-xyz");

        // Candidate with empty tracks (unfetched).
        let mut cand = candidate_with_recording(1, "rec-xyz");
        cand.release.tracks.clear();
        let g = mb_group("New", vec![track("A", Some(1), "/in/a.flac")], cand);
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert!(
            conflicts.is_empty(),
            "empty tracklist → MBID pass skipped, no conflicts"
        );
    }

    // ── Album-title fallback pass ─────────────────────────────────────

    /// Same as `track` but lets the caller override disc_number on both
    /// the Track and the TagData — needed to reproduce "two copies of
    /// the same album tagged with disagreeing disc numbers".
    fn track_with_disc(
        title: &str,
        track_number: Option<u32>,
        disc: u32,
        path: &str,
    ) -> (Track, Option<TagData>) {
        let (mut t, td) = track(title, track_number, path);
        t.disc_number = disc;
        let td = td.map(|mut t| {
            t.disc_number = Some(disc);
            t
        });
        (t, td)
    }

    #[test]
    fn detects_intra_batch_title_conflict_when_disc_tags_disagree() {
        // Scenario: two groups of the same 40-track album imported from
        // different sources. Positions 1 aligns on disc=1, but position
        // "21" is tagged disc=1 pos=21 in group A and disc=2 pos=1 in
        // group B. Album-slot sees them as different keys. Title must
        // catch it.
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let g1 = group(
            "A",
            GroupAction::AcceptAsIs,
            vec![track_with_disc("Late Song", Some(21), 1, "/in/a/21.flac")],
        );
        let g2 = group(
            "B",
            GroupAction::AcceptAsIs,
            vec![track_with_disc("Late Song", Some(1), 2, "/in/b/2-01.flac")],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();
        assert_eq!(
            conflicts.len(),
            1,
            "title pass must catch disc-divergent dup"
        );
        assert_eq!(conflicts[0].signal, DupSignal::AlbumTitle);
        match &conflicts[0].other {
            DupOther::Batch(earlier) => {
                assert_eq!(earlier.group, 0);
                assert_eq!(conflicts[0].new.group, 1);
            }
            _ => panic!("expected Batch conflict"),
        }
    }

    #[test]
    fn title_pass_fires_even_when_album_is_new() {
        // No DB album row at all. Pass 1 returns None for both groups;
        // pass 3 uses the tag album|artist key and still catches the
        // intra-batch dup.
        let conn = db::open_memory().unwrap();
        let g1 = group(
            "A",
            GroupAction::AcceptAsIs,
            vec![track("Only Song", Some(1), "/in/a/01.flac")],
        );
        let g2 = group(
            "B",
            GroupAction::AcceptAsIs,
            vec![track_with_disc("Only Song", Some(1), 2, "/in/b/01.flac")],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].signal, DupSignal::AlbumTitle);
    }

    #[test]
    fn title_pass_skipped_when_album_slot_already_flagged() {
        // Same album, same disc, same position, same title. Album-slot
        // raises a single conflict; title pass must not double-count.
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let g = group(
            "A",
            GroupAction::AcceptAsIs,
            vec![
                track("Same Title", Some(5), "/in/a.flac"),
                track("Same Title", Some(5), "/in/b.flac"),
            ],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert_eq!(conflicts.len(), 1, "one conflict, not two");
        assert_eq!(conflicts[0].signal, DupSignal::AlbumSlot);
    }

    #[test]
    fn title_pass_ignores_different_titles_at_same_slot() {
        // Two tracks at the same album but genuinely different titles —
        // e.g. disc 1 track 21 on group A, disc 2 track 1 on group B,
        // both for the same album but actually different songs. Pass 3
        // must not conflate them just because the album matches.
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let g1 = group(
            "A",
            GroupAction::AcceptAsIs,
            vec![track_with_disc("Song Alpha", Some(21), 1, "/in/a.flac")],
        );
        let g2 = group(
            "B",
            GroupAction::AcceptAsIs,
            vec![track_with_disc("Song Beta", Some(1), 2, "/in/b.flac")],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();
        assert!(conflicts.is_empty(), "different titles → no conflict");
    }

    #[test]
    fn title_pass_skips_empty_titles() {
        // Two groups each with a title-less track — must not collapse
        // under the empty-string key.
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let mut t1 = track("", Some(99), "/in/a.flac");
        if let Some(td) = &mut t1.1 {
            td.title = Some(String::new());
        }
        t1.0.title = String::new();
        let mut t2 = track("", Some(100), "/in/b.flac");
        if let Some(td) = &mut t2.1 {
            td.title = Some(String::new());
        }
        t2.0.title = String::new();

        let g1 = group("A", GroupAction::AcceptAsIs, vec![t1]);
        let g2 = group("B", GroupAction::AcceptAsIs, vec![t2]);
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();
        assert!(
            conflicts.is_empty(),
            "empty titles → no title-pass conflict"
        );
    }

    /// Build an MB candidate whose release has `per_disc * discs` tracks.
    /// Each disc's positions run 1..=per_disc (mirroring MB's per-disc
    /// position numbering — the exact shape that tripped the old
    /// position-only recording_id lookup on multi-disc releases).
    /// `recording_id` is unique per (disc, position).
    fn candidate_multi_disc(discs: u32, per_disc: u32) -> MbCandidate {
        let mut tracks = Vec::with_capacity((discs * per_disc) as usize);
        for d in 1..=discs {
            for p in 1..=per_disc {
                tracks.push(MbTrack {
                    disc: d,
                    position: p,
                    title: format!("D{}T{}", d, p),
                    artist: None,
                    duration_ms: None,
                    recording_id: format!("rec-d{}-p{}", d, p),
                });
            }
        }
        let release = MbRelease {
            id: "release-multi".to_string(),
            title: "Album".to_string(),
            artist: "Artist".to_string(),
            year: None,
            country: None,
            label: None,
            status: None,
            track_count: discs * per_disc,
            medium_count: discs,
            tracks,
            api_score: 0,
            release_group_id: None,
            group_min_year: None,
        };
        MbCandidate {
            release,
            score: MatchScore {
                total: 0.0,
                artist: 0.0,
                album: 0.0,
                track_count: 0.0,
                year: 0.0,
                duration: 0.0,
                tracks: 0.0,
                country: 0.0,
                original: 0.0,
            },
        }
    }

    #[test]
    fn mbid_pass_handles_multi_disc_without_phantom_intra_group_dups() {
        // The regression this guards: MB releases with multiple discs
        // carry per-disc positions (1..N per disc, not 1..(discs*N)
        // globally). The old `recording_id_for_batch_track` matched MB
        // tracks by position alone, so for a 2-disc 20+20 album every
        // local disc-2 track at pos P would inherit the disc-1 pos P
        // recording_id — producing 20 false intra-group MBID conflicts
        // per group (and 40 inter-group), i.e. "40 → 60 conflicts" once
        // the async release fetch lands.
        let conn = db::open_memory().unwrap();
        let cand = candidate_multi_disc(2, 3);

        // Local group: 3 tracks on disc 1 + 3 on disc 2, all with
        // per-disc positions 1..=3. Two groups of the same album —
        // simulating two source dirs dropped into one import.
        let make_tracks = |prefix: &str| {
            let mut v = Vec::new();
            for d in 1u32..=2 {
                for p in 1u32..=3 {
                    v.push(track_with_disc(
                        &format!("D{}T{}", d, p),
                        Some(p),
                        d,
                        &format!("/in/{}/d{}-p{}.flac", prefix, d, p),
                    ));
                }
            }
            v
        };
        let g1 = mb_group("A", make_tracks("a"), cand.clone());
        let g2 = mb_group("B", make_tracks("b"), cand);
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();

        // Every disc/pos combo appears exactly twice (once per group),
        // so we expect exactly 6 inter-group conflicts — and zero
        // intra-group.
        assert_eq!(
            conflicts.len(),
            6,
            "6 inter-group dup pairs, no intra-group phantoms"
        );
        for c in &conflicts {
            match &c.other {
                DupOther::Batch(earlier) => {
                    assert_ne!(
                        earlier.group, c.new.group,
                        "conflict within a single group is the old bug resurfacing"
                    );
                }
                _ => panic!("expected intra-batch conflicts"),
            }
        }
    }

    #[test]
    fn title_pass_cross_group_pairs_by_position_when_titles_repeat() {
        // Two groups of an album where several consecutive tracks share
        // the same TITLE tag (differentiation lives in the filename —
        // e.g. 4× "Let me battle" at positions 1..4). The pos-preferring
        // tier must pair each later-group track with its position twin,
        // not collapse all four onto the first earlier-group track.
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let make = |prefix: &str| {
            vec![
                track("Song", Some(1), &format!("/in/{}/1.flac", prefix)),
                track("Song", Some(2), &format!("/in/{}/2.flac", prefix)),
                track("Song", Some(3), &format!("/in/{}/3.flac", prefix)),
                track("Song", Some(4), &format!("/in/{}/4.flac", prefix)),
            ]
        };
        let g1 = group("A", GroupAction::AcceptAsIs, make("a"));
        let g2 = group("B", GroupAction::AcceptAsIs, make("b"));
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();

        assert_eq!(conflicts.len(), 4, "one conflict per later-group track");
        // Each later-group track must pair with the earlier-group track
        // at the SAME position, not with g0[0] four times.
        for c in &conflicts {
            assert_eq!(c.new.group, 1);
            match c.other {
                DupOther::Batch(earlier) => {
                    assert_eq!(earlier.group, 0);
                    assert_eq!(
                        earlier.index, c.new.index,
                        "later-group track at index {} should pair with earlier-group track at the same index",
                        c.new.index
                    );
                }
                _ => panic!("expected Batch conflict"),
            }
        }
    }

    #[test]
    fn title_pass_ignores_intra_group_same_title_tracks() {
        // Real-world case: a single album with multiple versions of the
        // same song at different track slots — e.g. "Let me battle" on
        // tracks 1/2/3/4 where the TITLE tag is identical and the
        // differentiation lives in the filename (feat. X, feat. Y, ...).
        // Pass 3 must not collapse them — that's the multi-version
        // false-positive the cross-group restriction guards against.
        let conn = db::open_memory().unwrap();
        queries::get_or_create_album(&conn, "Album", Some("Artist"), None, None, 11).unwrap();

        let g = group(
            "Album",
            GroupAction::AcceptAsIs,
            vec![
                track("Song", Some(1), "/in/01.flac"),
                track("Song", Some(2), "/in/02.flac"),
                track("Song", Some(3), "/in/03.flac"),
                track("Song", Some(4), "/in/04.flac"),
            ],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g]).unwrap();
        assert!(
            conflicts.is_empty(),
            "same-title tracks in a single group are legit alt-versions, not dups"
        );
    }

    #[test]
    fn title_pass_matches_mixed_actions_via_tag_key() {
        // One AcceptMb group and one AcceptAsIs group, same release.
        // DB has no album row yet (pass 1 returns None). The AcceptMb
        // group emits a `tag:album|artist` fallback key, which aligns
        // with the AcceptAsIs group's tag key — title pass catches.
        let conn = db::open_memory().unwrap();

        let g1 = mb_group(
            "A",
            vec![track("Shared", Some(1), "/in/a.flac")],
            candidate_with_recording(1, ""),
        );
        // Clear the candidate's tracks so the MBID pass is a no-op —
        // we want title pass to be the one that fires.
        let mut g1 = g1;
        g1.mb_candidates[0].release.tracks.clear();

        let g2 = group(
            "B",
            GroupAction::AcceptAsIs,
            vec![track("Shared", Some(1), "/in/b.flac")],
        );
        let conflicts = detect(&conn, std::path::Path::new(""), &[g1, g2]).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].signal, DupSignal::AlbumTitle);
    }
}
