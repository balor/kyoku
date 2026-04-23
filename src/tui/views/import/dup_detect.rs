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
//! v1 uses positional identity: the `(album_id, disc, position)` tuple,
//! where album_id is the one we'd *commit to* for the track, and position
//! comes from the track_number tag (same source the worker uses). That
//! misses MBID-equivalent tracks whose positions disagree, but covers the
//! common "partial then full re-import" case directly.
//!
//! Conflicts are returned, not resolved — the wizard renders a picker and
//! collects a `ConflictDecision` per conflict, which the worker then
//! applies.
//!
//! Out of scope for v1:
//!   * MBID-based conflict detection (would require pre-fetching the MB
//!     release in the wizard; currently happens inside the worker).
//!   * Fuzzy title/duration matching for as-is imports.

use std::collections::HashMap;

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
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub new: BatchTrackRef,
    pub other: DupOther,
    /// Why we think this is a duplicate. Kept for future rendering (we
    /// only have one variant in v1 so it isn't surfaced yet — but the
    /// field defines the shape for MBID-based detection later).
    #[allow(dead_code)]
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
/// Scans every non-skipped group. For each track, computes the
/// `(target_album_id, disc, position)` triple. If that slot is already
/// claimed (by an existing library row OR by an earlier batch track),
/// emits a conflict.
pub fn detect(conn: &Connection, groups: &[ImportGroup]) -> Result<Vec<Conflict>> {
    let mut conflicts = Vec::new();
    // Maps (album_id, disc, position) → first batch track that claimed it.
    // Used to catch intra-batch dupes.
    let mut batch_slots: HashMap<(i64, u32, u32), BatchTrackRef> = HashMap::new();

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
            } else {
                batch_slots.insert(key, new_ref);
            }
        }
    }

    Ok(conflicts)
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
}
