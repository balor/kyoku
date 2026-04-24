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
//! Detection runs two passes per invocation:
//!   1. **Album-slot**: `(album_id, disc, position)` — fast, DB-only, and
//!      catches the "partial then full re-import" case.
//!   2. **Recording MBID**: for AcceptMb groups whose selected candidate
//!      has a fetched tracklist, the MB `recording_id` at the local
//!      track's position is matched against `tracks.mbid` in the library
//!      and against other batch tracks. Skipped for (group, track) pairs
//!      that album-slot already flagged — MBID only *disambiguates*, it
//!      never replaces the slot signal.
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
            // Use the first track's tag-derived album (title + album_artist
            // or artist). The worker itself only commits an album when the
            // group is a "real album" — but for dup detection we use
            // whatever album_id would be found, and miss is fine (it just
            // means we return None and no AlbumSlot dup is raised for
            // this group).
            let Some((_, Some(td))) = group.tracks.first() else {
                return Ok(None);
            };
            let Some(album) = td.album.as_deref() else {
                return Ok(None);
            };
            let album_artist = td
                .album_artist
                .as_deref()
                .or(td.artist.as_deref());
            lookup_album(conn, album, album_artist)
        }
        GroupAction::Loose | GroupAction::Skip => Ok(None),
    }
}

/// Find an existing album row by title + album_artist. Returns None if
/// absent — detection then treats the group as "new album, nothing to
/// clash with in the library".
fn lookup_album(
    conn: &Connection,
    title: &str,
    album_artist: Option<&str>,
) -> Result<Option<i64>> {
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
pub fn detect(conn: &Connection, groups: &[ImportGroup]) -> Result<Vec<Conflict>> {
    let mut conflicts = Vec::new();
    // Maps (album_id, disc, position) → first batch track that claimed it.
    // Used to catch intra-batch dupes.
    let mut batch_slots: HashMap<(i64, u32, u32), BatchTrackRef> = HashMap::new();
    // (group_idx, track_idx) pairs already flagged by the album-slot pass —
    // the MBID pass skips them.
    let mut flagged: HashSet<(usize, usize)> = HashSet::new();

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
            let new_ref = BatchTrackRef { group: gi, index: ti };

            // Library dup?
            if let Some(existing) =
                queries::find_track_by_album_slot(conn, album_id, disc, pos)?
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
    let mut batch_mbids: HashMap<String, BatchTrackRef> = HashMap::new();
    for (gi, group) in groups.iter().enumerate() {
        if matches!(group.action, GroupAction::Skip | GroupAction::Loose) {
            continue;
        }
        for (ti, _) in group.tracks.iter().enumerate() {
            if flagged.contains(&(gi, ti)) {
                continue;
            }
            let Some(recording_id) = recording_id_for_batch_track(group, ti) else {
                continue;
            };
            let new_ref = BatchTrackRef { group: gi, index: ti };

            // Library dup by MBID?
            if let Some(existing) = queries::find_track_by_mbid(conn, &recording_id)? {
                conflicts.push(Conflict {
                    new: new_ref,
                    other: DupOther::Library(existing),
                    signal: DupSignal::Mbid,
                });
                batch_mbids.entry(recording_id).or_insert(new_ref);
                continue;
            }

            // Intra-batch dup by MBID?
            if let Some(earlier) = batch_mbids.get(&recording_id) {
                conflicts.push(Conflict {
                    new: new_ref,
                    other: DupOther::Batch(*earlier),
                    signal: DupSignal::Mbid,
                });
            } else {
                batch_mbids.insert(recording_id, new_ref);
            }
        }
    }

    Ok(conflicts)
}

/// Resolve the MB `recording_id` that would be assigned to this local
/// track at import time, if we have enough data. Returns `None` for
/// groups that aren't AcceptMb, have no selected candidate, have an
/// unfetched release (empty `tracks`), or whose local track has no
/// resolvable position in the MB tracklist.
fn recording_id_for_batch_track(group: &ImportGroup, track_idx: usize) -> Option<String> {
    if group.action != GroupAction::AcceptMb {
        return None;
    }
    let cand_idx = group.selected_candidate?;
    let cand = group.mb_candidates.get(cand_idx)?;
    if cand.release.tracks.is_empty() {
        return None;
    }
    let (local, _) = group.tracks.get(track_idx)?;
    let pos = local.track_number?;
    if pos == 0 {
        return None;
    }
    // MbTrack has no disc field (MB returns per-disc tracklists but our
    // parser flattens positions). Fall back to matching on position
    // alone — the same approximation album-slot uses.
    let mb_track = cand.release.tracks.iter().find(|t| t.position == pos)?;
    if mb_track.recording_id.is_empty() {
        None
    } else {
        Some(mb_track.recording_id.clone())
    }
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
        };
        (t, Some(td))
    }

    fn group(name: &str, action: GroupAction, tracks: Vec<(Track, Option<TagData>)>) -> ImportGroup {
        ImportGroup {
            name: name.to_string(),
            tracks,
            action,
            mb_candidates: Vec::new(),
            selected_candidate: None,
            mb_state: MbMatchState::NotStarted,
            target_collection: String::new(),
            full_release_fetching: false,
        }
    }

    /// Insert a pre-existing track into the library at album "Album" /
    /// artist "Artist", position `pos`. Returns the inserted track id.
    fn seed_library_track(conn: &Connection, pos: u32, path: &str) -> i64 {
        let (album_id, _) = queries::get_or_create_album(
            conn,
            "Album",
            Some("Artist"),
            None,
            None,
            11,
        )
        .unwrap();
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
        queries::insert_track(conn, &track, Some(album_id), Some(10_000)).unwrap()
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
        let conflicts = detect(&conn, &[g]).unwrap();
        assert_eq!(conflicts.len(), 1);
        match &conflicts[0].other {
            DupOther::Library(e) => assert_eq!(e.file_path, "/lib/05.flac"),
            _ => panic!("expected Library conflict"),
        }
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
        let conflicts = detect(&conn, &[g]).unwrap();
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
        let conflicts = detect(&conn, &[g]).unwrap();
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
        let conflicts = detect(&conn, &[g_skip, g_loose]).unwrap();
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
        let conflicts = detect(&conn, &[g]).unwrap();
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
            track_count: 1,
            tracks: vec![MbTrack {
                position: pos,
                title: format!("Track {}", pos),
                artist: None,
                duration_ms: None,
                recording_id: recording_id.to_string(),
            }],
            api_score: 0,
            release_group_id: None,
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
            },
        }
    }

    fn mb_group(name: &str, tracks: Vec<(Track, Option<TagData>)>, cand: MbCandidate) -> ImportGroup {
        ImportGroup {
            name: name.to_string(),
            tracks,
            action: GroupAction::AcceptMb,
            mb_candidates: vec![cand],
            selected_candidate: Some(0),
            mb_state: MbMatchState::Done,
            target_collection: String::new(),
            full_release_fetching: false,
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
    fn detects_library_conflict_by_mbid_when_album_slot_misses() {
        let conn = db::open_memory().unwrap();
        // Seed a library row with MBID but at a *different* album title
        // — album-slot can't match, MBID must.
        let (album_id, _) = queries::get_or_create_album(
            &conn,
            "Different Album",
            Some("Artist"),
            None,
            None,
            1,
        )
        .unwrap();
        let existing_id = queries::insert_track(
            &conn,
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

        // Incoming: AcceptMb group, candidate's MB track at pos 5 has
        // recording_id "rec-abc". Local track is at pos 5 too.
        let g = mb_group(
            "New Album",
            vec![track("New", Some(5), "/in/05.flac")],
            candidate_with_recording(5, "rec-abc"),
        );
        let conflicts = detect(&conn, &[g]).unwrap();
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
        let conflicts = detect(&conn, &[g1, g2]).unwrap();
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
        let conflicts = detect(&conn, &[g]).unwrap();
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
        let (album_id, _) = queries::get_or_create_album(
            &conn,
            "Other",
            Some("Artist"),
            None,
            None,
            1,
        )
        .unwrap();
        let existing_id = queries::insert_track(
            &conn,
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
        let g = mb_group(
            "New",
            vec![track("A", Some(1), "/in/a.flac")],
            cand,
        );
        let conflicts = detect(&conn, &[g]).unwrap();
        assert!(
            conflicts.is_empty(),
            "empty tracklist → MBID pass skipped, no conflicts"
        );
    }
}
