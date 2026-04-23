//! Background import worker. Runs on its own thread with its own DB
//! connection so we don't block the TUI's main-thread `Connection`.

use std::sync::mpsc;

use lofty::tag::ItemKey;

use crate::config::settings::NameScriptPreference;
use crate::core::importer::detect_sibling_cover;
use crate::core::tagger::{self, TagChanges, TagData, TagValue};
use crate::db::models::Track;
use crate::db::queries;
use crate::external::musicbrainz::{MbClient, MbRelease, MbTrack};

use super::{GroupAction, ImportGroup, ImportMessage};

pub(super) fn run_import_worker(
    groups_to_import: Vec<ImportGroup>,
    // Parallel to `groups_to_import`: `plans[gi][ti]` is the per-track
    // decision from the duplicate-resolution step. Empty inner vecs (or an
    // empty outer vec) mean "no plans — insert everything normally".
    plans: Vec<Vec<super::dup_detect::BatchTrackPlan>>,
    user_skipped: u32,
    rate_limit_ms: u64,
    name_script: NameScriptPreference,
    write_tags: bool,
    tx: mpsc::Sender<ImportMessage>,
) {
    let conn = match crate::db::open_database(crate::config::paths::database_file()) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(ImportMessage::Complete(format!("DB open failed: {}", e)));
            return;
        }
    };
    let conn = &conn;

    let total_tracks: usize = groups_to_import.iter().map(|g| g.tracks.len()).sum();
    let mut done = 0usize;
    let mut imported = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;
    let mut added_to_collection = 0u32;
    // Counters for the duplicate-resolution outcome — surfaced in the
    // final summary so the user sees what their picks did.
    let mut dup_replaced = 0u32;
    let mut dup_user_skipped = 0u32;
    let mut orphaned = 0u32;

    // Fetch full release data for MB-matched groups (search results don't
    // include track listings — we need them for per-track metadata).
    let mut mb_client = MbClient::new(rate_limit_ms, name_script);

    {
        let _ = tx.send(ImportMessage::Progress(done, total_tracks));
        for (gi, group) in groups_to_import.iter().enumerate() {
            let loose = group.action == GroupAction::Loose;
            let group_plans = plans.get(gi);

            // Resolve this group's target collection (if any)
            let target_collection_name = group.target_collection.trim();
            let target_collection_id = if !target_collection_name.is_empty() {
                queries::get_or_create_collection(conn, target_collection_name)
                    .ok()
                    .map(|(id, _)| id)
            } else {
                None
            };

            // If MB match, fetch the full release once for the whole group
            let mb_full = if group.action == GroupAction::AcceptMb {
                group
                    .selected_candidate
                    .and_then(|idx| group.mb_candidates.get(idx))
                    .and_then(|c| mb_client.fetch_release(&c.release.id).ok())
            } else {
                None
            };

            // For MB-matched groups, all tracks share one album (the MB release).
            // Precompute it once.
            let mb_album_id = if loose {
                None
            } else if let Some(mb) = &mb_full {
                queries::get_or_create_album(
                    conn,
                    &mb.title,
                    Some(&mb.artist),
                    mb.year,
                    None,
                    group.tracks.len() as u32,
                )
                .ok()
                .map(|(id, _)| id)
            } else {
                None
            };

            // Record sibling cover art next to the audio, if present. Runs
            // once per group and only when we actually created/found an
            // album row — loose tracks don't have albums to attach to.
            if let Some(aid) = mb_album_id {
                stamp_sibling_cover(conn, aid, group);
            }

            // Update MB album with MB metadata
            if let (Some(aid), Some(mb)) = (mb_album_id, &mb_full) {
                queries::update_album_mb(
                    conn,
                    aid,
                    &mb.id,
                    &mb.artist,
                    &mb.title,
                    mb.year,
                    mb.label.as_deref(),
                )
                .ok();
            }

            // For "Accept as-is" mode, decide if this group is a real album
            // (all tracks share the same album+album_artist tags) or a compilation.
            // Compilations get NO per-track albums — those tracks become loose
            // and live entirely in their assigned collection (if any).
            //
            // Rules for "real album":
            // 1. All tracks share the same (album, album_artist) tuple
            // 2. The album_artist is NOT a "various" marker
            //    (Various, Various Artists, VA, Compilation, etc.)
            // 3. Track-level artists are not wildly diverse
            //    (compilations often have one unique artist per track)
            let group_is_real_album = !loose && mb_full.is_none() && {
                let key = |td: &Option<tagger::TagData>| -> Option<(String, Option<String>)> {
                    td.as_ref().and_then(|t| {
                        t.album.as_ref().map(|a| {
                            (
                                a.clone(),
                                t.album_artist.clone().or_else(|| t.artist.clone()),
                            )
                        })
                    })
                };
                let first_key = group.tracks.first().and_then(|(_, td)| key(td));
                let consistent_album_key =
                    first_key.is_some() && group.tracks.iter().all(|(_, td)| key(td) == first_key);

                if !consistent_album_key {
                    false
                } else {
                    // Check rule 2: album_artist isn't a "various" marker
                    let album_artist_lower = first_key
                        .as_ref()
                        .and_then(|(_, aa)| aa.as_ref())
                        .map(|s| s.to_lowercase())
                        .unwrap_or_default();
                    let is_various_marker = matches!(
                        album_artist_lower.as_str(),
                        "various"
                            | "various artists"
                            | "various artist"
                            | "va"
                            | "v.a."
                            | "compilation"
                            | "compilations"
                            | "soundtrack"
                            | "ost"
                    );

                    if is_various_marker {
                        false
                    } else {
                        // Check rule 3: track-level artist diversity
                        // Real albums typically have 1-3 distinct artists (main + features).
                        // If >= 4 distinct artists OR >= 40% of tracks have unique artists,
                        // it's a compilation.
                        use std::collections::HashSet;
                        let unique_artists: HashSet<String> = group
                            .tracks
                            .iter()
                            .filter_map(|(_, td)| {
                                td.as_ref().and_then(|t| t.artist.as_ref()).cloned()
                            })
                            .collect();
                        let n_tracks = group.tracks.len().max(1);
                        let too_diverse = unique_artists.len() >= 4
                            || (unique_artists.len() * 100 / n_tracks) >= 40;
                        !too_diverse
                    }
                }
            };

            // Precompute the as-is album once if we determined this is a real album
            let asis_album_id = if group_is_real_album {
                let first_tag = group.tracks.first().and_then(|(_, td)| td.as_ref());
                first_tag.and_then(|td| {
                    td.album.as_ref().and_then(|album_title| {
                        queries::get_or_create_album(
                            conn,
                            album_title,
                            td.album_artist.as_deref().or(td.artist.as_deref()),
                            td.year.map(|y| y as i32),
                            td.genre.as_deref(),
                            group.tracks.len() as u32,
                        )
                        .ok()
                        .map(|(id, _)| id)
                    })
                })
            } else {
                None
            };

            if let Some(aid) = asis_album_id {
                stamp_sibling_cover(conn, aid, group);
            }

            // Pair each local track to an MB track once, up front. We need
            // the pairing to survive iteration order (partial album like
            // "tracks 5-11 of an 11-track release" has enumeration indices
            // 0-6 — matching by index would tag them with MB positions 1-7
            // instead of 5-11). Pairing uses track_number tags first, then
            // title similarity (beets-style), then positional as a last
            // resort. See `match_group_to_mb` for the full policy.
            let mb_pairing: Vec<Option<usize>> = if let Some(mb) = &mb_full {
                match_group_to_mb(&group.tracks, &mb.tracks)
            } else {
                Vec::new()
            };

            for (i, (track, _tag_data)) in group.tracks.iter().enumerate() {
                let path_str = track.file_path.display().to_string();

                // Duplicate-resolution plan for this track, if any. Must
                // happen before the path-existence check because the user
                // may have explicitly chosen to replace an existing row
                // (its path could collide with the incoming file's path,
                // though usually they're different on disk).
                let plan = group_plans.and_then(|gp| gp.get(i));

                if let Some(p) = plan
                    && p.skip
                {
                    dup_user_skipped += 1;
                    done += 1;
                    let _ = tx.send(ImportMessage::Progress(done, total_tracks));
                    continue;
                }

                // Apply a "replace" decision: delete the existing row
                // (its file stays on disk) and log the path as an orphan
                // for the next organize pass to clean up.
                if let Some(p) = plan
                    && let Some(repl) = p.replace_existing.as_ref()
                {
                    if let Err(e) = queries::delete_track(conn, repl.id) {
                        tracing::warn!("dup replace: delete_track({}) failed: {}", repl.id, e);
                    } else {
                        dup_replaced += 1;
                    }
                    if let Err(e) = queries::insert_orphan(
                        conn,
                        &repl.file_path,
                        Some(&repl.title),
                        repl.artist.as_deref(),
                        repl.album_title.as_deref(),
                        "replaced by duplicate during import",
                    ) {
                        tracing::warn!(
                            "dup replace: insert_orphan({}) failed: {}",
                            repl.file_path,
                            e
                        );
                    } else {
                        orphaned += 1;
                    }
                }

                if queries::track_exists_by_path(conn, &path_str).unwrap_or(false) {
                    skipped += 1;
                    done += 1;
                    let _ = tx.send(ImportMessage::Progress(done, total_tracks));
                    continue;
                }

                // Per-track album resolution:
                // - Loose: no album
                // - MB-matched: use the precomputed MB album (one per group)
                // - Real album (as-is): use the precomputed shared album
                // - Compilation (as-is): no album → tracks become loose, but
                //   they can still be in a collection
                let album_id = if loose {
                    None
                } else if let Some(id) = mb_album_id {
                    Some(id)
                } else { asis_album_id.map(|id| id) };

                let file_size = std::fs::metadata(&track.file_path)
                    .map(|m| m.len() as i64)
                    .ok();

                match queries::insert_track(conn, track, album_id, file_size) {
                    Ok(track_id) => {
                        imported += 1;

                        // Apply per-track MB data (title, artist, recording MBID)
                        if let Some(mb) = &mb_full {
                            let mb_track = mb_pairing
                                .get(i)
                                .and_then(|o| *o)
                                .and_then(|idx| mb.tracks.get(idx));
                            if let Some(mbt) = mb_track {
                                queries::update_track_mb(
                                    conn,
                                    track_id,
                                    &mbt.recording_id,
                                    mbt.artist.as_deref().unwrap_or(&mb.artist),
                                    &mbt.title,
                                    "matched",
                                )
                                .ok();

                                // Mirror the MB match to the file's tags. DB is
                                // now authoritative either way, but beets-style
                                // behaviour is to keep the file in sync so the
                                // library is portable to other tools.
                                if write_tags {
                                    let changes = build_mb_tag_changes(mb, mbt);
                                    if let Err(e) =
                                        tagger::write_tags(&track.file_path, &changes)
                                    {
                                        tracing::warn!(
                                            "tag write failed for {}: {}",
                                            track.file_path.display(),
                                            e
                                        );
                                    }
                                }
                            } else {
                                // No positional match — still mark as matched at album level
                                queries::set_track_tag_status(conn, track_id, "matched")
                                    .ok();
                            }
                        }

                        // Add to target collection if user requested one
                        if let Some(coll_id) = target_collection_id
                            && queries::add_track_to_collection(conn, coll_id, track_id)
                                .unwrap_or(false)
                            {
                                added_to_collection += 1;
                            }
                    }
                    Err(_) => errors += 1,
                }
                done += 1;
                let _ = tx.send(ImportMessage::Progress(done, total_tracks));
            }
        }
    }

    let mut parts = vec![format!("Imported: {}", imported)];
    if added_to_collection > 0 {
        parts.push(format!("Added to collections: {}", added_to_collection));
    }
    if skipped > 0 {
        // "Duplicates" here means "file path already in DB". Keep the
        // label as-is so old users recognise it; the new resolution
        // counters below use distinct wording.
        parts.push(format!("Duplicates: {}", skipped));
    }
    if dup_replaced > 0 {
        parts.push(format!("Replaced: {}", dup_replaced));
    }
    if dup_user_skipped > 0 {
        parts.push(format!("Dup-skipped: {}", dup_user_skipped));
    }
    if orphaned > 0 {
        // The replaced files are still on disk — mention it so the user
        // knows the next organize pass will clean them up.
        parts.push(format!(
            "Orphaned: {} (will be removed on next organize)",
            orphaned
        ));
    }
    if user_skipped > 0 {
        parts.push(format!("Skipped: {}", user_skipped));
    }
    if errors > 0 {
        parts.push(format!("Errors: {}", errors));
    }
    let _ = tx.send(ImportMessage::Complete(parts.join(", ")));
}

/// Pull a 1-based track position out of a filename's leading digits
/// (e.g. `05. Foo.flac` → 5, `12 - Bar.mp3` → 12). Returns `None` if
/// there are no leading digits or the number is 0. Used only as a
/// fallback when the track_number tag is absent.
fn parse_filename_position(path: &std::path::Path) -> Option<u32> {
    let stem = path.file_stem()?.to_str()?;
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok().filter(|n| *n > 0)
}

/// Strip common filename-boilerplate prefixes so a filename-derived title
/// can be compared against an MB track title. Removes a leading `NN` /
/// `NN.` / `NN -` sequence followed by a single `Artist - ` prefix.
/// Called only when the title tag is absent (i.e. `track.title` is a raw
/// file stem).
fn strip_filename_title_prefixes(s: &str) -> String {
    let after_digits = s.trim_start().trim_start_matches(|c: char| c.is_ascii_digit());
    let after_sep =
        after_digits.trim_start_matches(|c: char| c == '.' || c == '-' || c == '_' || c == ' ');
    // Drop one "Something - " prefix if present (typical Artist separator).
    if let Some((_, tail)) = after_sep.split_once(" - ") {
        tail.trim().to_string()
    } else {
        after_sep.trim().to_string()
    }
}

/// Pair each local track in a group to an MB track. Returns a vec parallel
/// to `group_tracks`; `matches[i] = Some(mi)` means local track `i` maps to
/// `mb_tracks[mi]`, and `None` means we couldn't confidently pair it.
///
/// Tag values are the authoritative signal throughout. Filename-derived
/// hints only come into play when the corresponding tag is **absent** —
/// they never override a present tag (even a wrong one). If a user has
/// corrupted tags, that's an unrecoverable input-side problem, and
/// silently second-guessing present tags would just hide it.
///
/// Matching runs in three passes, each claiming MB tracks greedily so later
/// passes can't steal them:
///
///   1. **By track-number** — tag track number when present. If absent,
///      parse leading digits from the filename (`05. Foo.flac` → 5).
///      Handles the "partial album" case (files 5-11 of an 11-track
///      release) directly.
///   2. **By title similarity** — Jaro-Winkler ≥ 0.85 against the MB
///      track title. When the title tag is absent, `track.title` is
///      already filename-derived (see `tagger::read_track`), so we strip
///      common `NN.` / `Artist -` prefixes before scoring to avoid
///      blowing the similarity score on boilerplate.
///   3. **Positional** — fill any remaining gap with the `(i+1)`-th MB
///      track if still available. Kept for the truly tag-less case.
///
/// This mirrors the tiered strategy beets uses (it does full bipartite
/// assignment with duration weighting, but for typical single-disc albums
/// greedy matching on the two strongest signals is equivalent in practice).
fn match_group_to_mb(
    group_tracks: &[(Track, Option<TagData>)],
    mb_tracks: &[MbTrack],
) -> Vec<Option<usize>> {
    let n = group_tracks.len();
    let mut matches: Vec<Option<usize>> = vec![None; n];
    let mut taken: Vec<bool> = vec![false; mb_tracks.len()];

    // Pass 1: track_number (tag first, filename as fallback only when tag
    // is absent) → MB position.
    for (li, (track, tag_data)) in group_tracks.iter().enumerate() {
        let tag_tn = tag_data.as_ref().and_then(|t| t.track_number);
        let tn = match tag_tn {
            Some(0) => continue, // bogus "0" — don't hijack anything
            Some(n) => n,
            None => match parse_filename_position(&track.file_path) {
                Some(n) => n,
                None => continue,
            },
        };
        for (mi, mt) in mb_tracks.iter().enumerate() {
            if taken[mi] {
                continue;
            }
            if mt.position == tn {
                matches[li] = Some(mi);
                taken[mi] = true;
                break;
            }
        }
    }

    // Pass 2: title similarity, greedy by best-score-first.
    // Collect (local_idx, mb_idx, score) for all plausible pairs, then
    // claim them in descending score order.
    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();
    for (li, (track, tag_data)) in group_tracks.iter().enumerate() {
        if matches[li].is_some() {
            continue;
        }
        // If the title tag is absent, `track.title` carries the raw file
        // stem (e.g. "05. 9Lana - Nandemoshitaikara"). Strip the usual
        // prefixes so the comparison is between just the song titles.
        let tag_title_present = tag_data
            .as_ref()
            .and_then(|t| t.title.as_deref())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let compare_title: String = if tag_title_present {
            track.title.clone()
        } else {
            strip_filename_title_prefixes(&track.title)
        };
        let local_title: String = compare_title
            .chars()
            .flat_map(|c| c.to_lowercase())
            .collect();
        for (mi, mt) in mb_tracks.iter().enumerate() {
            if taken[mi] {
                continue;
            }
            let mb_title: String = mt
                .title
                .chars()
                .flat_map(|c| c.to_lowercase())
                .collect();
            let score = strsim::jaro_winkler(&local_title, &mb_title);
            if score >= 0.85 {
                candidates.push((li, mi, score));
            }
        }
    }
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    for (li, mi, _) in candidates {
        if matches[li].is_some() || taken[mi] {
            continue;
        }
        matches[li] = Some(mi);
        taken[mi] = true;
    }

    // Pass 3: positional fallback — fills remaining gaps for fully
    // tag-less groups. Intentionally last so it can't overwrite a
    // title-based hit.
    for li in 0..n {
        if matches[li].is_some() {
            continue;
        }
        let want = (li + 1) as u32;
        for (mi, mt) in mb_tracks.iter().enumerate() {
            if taken[mi] {
                continue;
            }
            if mt.position == want {
                matches[li] = Some(mi);
                taken[mi] = true;
                break;
            }
        }
    }

    matches
}

/// Build the tag delta we apply to a file after a successful MB match.
/// Covers the core fields every format supports plus the two MBIDs we carry
/// through (release + recording). Other MBIDs aren't populated upstream yet,
/// and on ID3v2 the MB-prefixed keys are lossy via lofty's generic `Tag`
/// API — see the limitation note in `core::tagger` — so MP3 imports still
/// benefit from the non-MB frames even when the MBIDs silently drop.
fn build_mb_tag_changes(mb: &MbRelease, mbt: &MbTrack) -> TagChanges {
    let mut changes = TagChanges::default();
    let artist = mbt.artist.as_deref().unwrap_or(&mb.artist);

    changes
        .set
        .push((ItemKey::TrackTitle, TagValue::Text(mbt.title.clone())));
    changes
        .set
        .push((ItemKey::TrackArtist, TagValue::Text(artist.to_string())));
    changes
        .set
        .push((ItemKey::AlbumTitle, TagValue::Text(mb.title.clone())));
    changes
        .set
        .push((ItemKey::AlbumArtist, TagValue::Text(mb.artist.clone())));
    if let Some(year) = mb.year {
        changes
            .set
            .push((ItemKey::Year, TagValue::Text(year.to_string())));
    }
    changes.set.push((
        ItemKey::TrackNumber,
        TagValue::Text(mbt.position.to_string()),
    ));
    let total = if mb.track_count > 0 {
        mb.track_count
    } else {
        mb.tracks.len() as u32
    };
    if total > 0 {
        changes
            .set
            .push((ItemKey::TrackTotal, TagValue::Text(total.to_string())));
    }
    changes.set.push((
        ItemKey::MusicBrainzReleaseId,
        TagValue::Text(mb.id.clone()),
    ));
    changes.set.push((
        ItemKey::MusicBrainzRecordingId,
        TagValue::Text(mbt.recording_id.clone()),
    ));
    changes
}

/// Scan the group's source directory for a cover-art file and stamp it onto
/// the given album row. No-op if the group has no resolvable source dir or
/// no matching cover file sits next to the audio.
fn stamp_sibling_cover(conn: &rusqlite::Connection, album_id: i64, group: &ImportGroup) {
    let Some(source_dir) = group
        .tracks
        .first()
        .and_then(|(t, _)| t.source_dir.as_deref())
    else {
        return;
    };
    if let Some(cover) = detect_sibling_cover(source_dir) {
        let _ = queries::set_album_cover_path(conn, album_id, &cover.display().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{AudioFormat, TagStatus};
    use std::path::PathBuf;

    /// Build a (Track, TagData) pair where the tag data carries both the
    /// title and the track number — i.e. the "tags are present" case.
    fn local(title: &str, track_number: Option<u32>) -> (Track, Option<TagData>) {
        let t = Track {
            id: None,
            album_id: None,
            title: title.to_string(),
            artist: None,
            track_number,
            disc_number: 1,
            duration_ms: None,
            mbid: None,
            file_path: PathBuf::from(format!("/tmp/{}.mp3", title)),
            file_format: AudioFormat::Mp3,
            bitrate: None,
            sample_rate: None,
            tag_status: TagStatus::Unmatched,
            source_dir: None,
        };
        let td = TagData {
            title: Some(title.to_string()),
            artist: None,
            album: None,
            album_artist: None,
            year: None,
            track_number,
            disc_number: None,
            genre: None,
            duration: None,
        };
        (t, Some(td))
    }

    /// Build a (Track, TagData) pair simulating a fully tag-less file:
    /// `track.title` carries the raw file stem (as `tagger::read_track`
    /// does), and TagData's title/track_number are None.
    fn local_untagged(file_stem: &str) -> (Track, Option<TagData>) {
        let t = Track {
            id: None,
            album_id: None,
            title: file_stem.to_string(),
            artist: None,
            track_number: None,
            disc_number: 1,
            duration_ms: None,
            mbid: None,
            file_path: PathBuf::from(format!("/tmp/{}.mp3", file_stem)),
            file_format: AudioFormat::Mp3,
            bitrate: None,
            sample_rate: None,
            tag_status: TagStatus::Unmatched,
            source_dir: None,
        };
        let td = TagData {
            title: None,
            artist: None,
            album: None,
            album_artist: None,
            year: None,
            track_number: None,
            disc_number: None,
            genre: None,
            duration: None,
        };
        (t, Some(td))
    }

    fn mb(position: u32, title: &str) -> MbTrack {
        MbTrack {
            position,
            title: title.to_string(),
            artist: None,
            duration_ms: None,
            recording_id: String::new(),
        }
    }

    #[test]
    fn partial_album_matches_by_track_number() {
        // 7 local tracks carrying their real positions 5..=11; 11-track
        // MB release. The index-based loop would put them at MB 1..=7,
        // which was the actual bug. Pass 1 must pair them 5..=11.
        let group = vec![
            local("E", Some(5)),
            local("F", Some(6)),
            local("G", Some(7)),
            local("H", Some(8)),
            local("I", Some(9)),
            local("J", Some(10)),
            local("K", Some(11)),
        ];
        let mb_tracks: Vec<MbTrack> = (1..=11)
            .map(|p| mb(p, &format!("mb-title-{}", p)))
            .collect();

        let matches = match_group_to_mb(&group, &mb_tracks);

        let got: Vec<u32> = matches
            .iter()
            .map(|m| mb_tracks[m.unwrap()].position)
            .collect();
        assert_eq!(got, vec![5, 6, 7, 8, 9, 10, 11]);
    }

    #[test]
    fn title_similarity_rescues_missing_track_numbers() {
        // track_number tags absent across the board; titles match MB
        // titles exactly. Pass 2 must pair them up by title instead of
        // falling through to the positional fallback.
        let group = vec![
            local("Never Give Up (Instrumental)", None),
            local("Let me battle (Instrumental)", None),
            local("propose (Instrumental)", None),
        ];
        let mb_tracks = vec![
            mb(1, "Let me battle"),
            mb(2, "Never Give Up"),
            mb(3, "propose"),
            mb(4, "Let me battle (Instrumental)"),
            mb(5, "Never Give Up (Instrumental)"),
            mb(6, "propose (Instrumental)"),
        ];

        let matches = match_group_to_mb(&group, &mb_tracks);

        let got_titles: Vec<&str> = matches
            .iter()
            .map(|m| mb_tracks[m.unwrap()].title.as_str())
            .collect();
        assert_eq!(
            got_titles,
            vec![
                "Never Give Up (Instrumental)",
                "Let me battle (Instrumental)",
                "propose (Instrumental)",
            ]
        );
    }

    #[test]
    fn positional_fallback_only_when_nothing_else_matches() {
        // No track numbers, titles totally opaque (no similarity to MB
        // titles). Must fall through to positional — regression check
        // that the positional pass is *still* there for tag-less files.
        let group = vec![
            local("xxxxxxxxxxxxx", None),
            local("yyyyyyyyyyyyy", None),
        ];
        let mb_tracks = vec![
            mb(1, "Alpha"),
            mb(2, "Beta"),
            mb(3, "Gamma"),
        ];

        let matches = match_group_to_mb(&group, &mb_tracks);
        assert_eq!(matches, vec![Some(0), Some(1)]);
    }

    #[test]
    fn zero_track_number_is_ignored() {
        // Some rippers write "0" for track number on singles/unknowns.
        // Must not let a "0" hijack anything — should fall through to
        // title match.
        let group = vec![local("Alpha", Some(0))];
        let mb_tracks = vec![mb(1, "Alpha")];
        let matches = match_group_to_mb(&group, &mb_tracks);
        assert_eq!(matches, vec![Some(0)]);
    }

    fn local_untagged_at(stem: &str, dir_path: &str) -> (Track, Option<TagData>) {
        let mut t = local_untagged(stem);
        t.0.file_path = PathBuf::from(format!("{}/{}.flac", dir_path, stem));
        t
    }

    #[test]
    fn filename_position_used_when_track_number_tag_absent() {
        // Fully tag-less files (title tag None, track_number tag None).
        // Filenames carry positions 5..=7 — the filename-position
        // fallback should place them at MB 5/6/7, not the dumb
        // positional (i+1) = 1/2/3.
        let group = vec![
            local_untagged_at("05. 9Lana - Nandemoshitaikara", "/tmp/album"),
            local_untagged_at("06. 9Lana - Never Give Up", "/tmp/album"),
            local_untagged_at("07. 9Lana - propose", "/tmp/album"),
        ];
        let mb_tracks: Vec<MbTrack> = (1..=11)
            .map(|p| mb(p, &format!("track-{}", p)))
            .collect();

        let matches = match_group_to_mb(&group, &mb_tracks);
        let got: Vec<u32> = matches
            .iter()
            .map(|m| mb_tracks[m.unwrap()].position)
            .collect();
        assert_eq!(got, vec![5, 6, 7]);
    }

    #[test]
    fn filename_title_prefixes_stripped_for_similarity() {
        // No track_number tag, no title tag; track.title carries the
        // raw file stem. Pass 1 puts them by filename position, but
        // suppose positions collide — strip the digits-and-artist
        // boilerplate so Pass 2 would still recognise the song title.
        let group = vec![local_untagged("05. 9Lana - Nandemoshitaikara")];
        // Only one MB track, and its position doesn't match the
        // filename's "05". Pass 1 finds no position 5, Pass 2 must
        // score "Nandemoshitaikara" (after stripping "05. 9Lana - ")
        // against the MB title.
        let mb_tracks = vec![mb(1, "Nandemoshitaikara")];
        let matches = match_group_to_mb(&group, &mb_tracks);
        assert_eq!(matches, vec![Some(0)]);
    }

    #[test]
    fn present_title_tag_is_not_second_guessed_by_filename() {
        // Tags are PRESENT but wrong (the corrupt-import scenario).
        // Rule: tags stay authoritative — we must NOT reach for the
        // filename to "repair" them. Here the tag title wrongly says
        // "Let me battle" and the tag track_number wrongly says 1.
        // MB has a "Let me battle" at position 1. The matcher should
        // honour the (wrong) tags and pair local→MB 1, even though the
        // filename hints at position 5 / title "Nandemoshitaikara".
        let mut item = local("Let me battle", Some(1));
        item.0.file_path =
            PathBuf::from("/tmp/album/05. 9Lana - Nandemoshitaikara.flac");
        let group = vec![item];
        let mb_tracks = vec![
            mb(1, "Let me battle"),
            mb(5, "Nandemoshitaikara"),
        ];
        let matches = match_group_to_mb(&group, &mb_tracks);
        // Honour the wrong tags — pair to MB position 1, not 5.
        assert_eq!(matches, vec![Some(0)]);
    }

    #[test]
    fn strip_filename_title_prefixes_cases() {
        assert_eq!(
            strip_filename_title_prefixes("05. 9Lana - Nandemoshitaikara"),
            "Nandemoshitaikara"
        );
        assert_eq!(
            strip_filename_title_prefixes("12 - Never Give Up"),
            "Never Give Up"
        );
        // Digit-dot-only, no artist prefix
        assert_eq!(strip_filename_title_prefixes("07. Song"), "Song");
        // Already clean — untouched
        assert_eq!(strip_filename_title_prefixes("Nandemoshitaikara"), "Nandemoshitaikara");
    }

    #[test]
    fn parse_filename_position_cases() {
        use std::path::Path;
        assert_eq!(
            parse_filename_position(Path::new("/x/05. 9Lana - Foo.flac")),
            Some(5)
        );
        assert_eq!(
            parse_filename_position(Path::new("/x/12 - Bar.mp3")),
            Some(12)
        );
        // No leading digits
        assert_eq!(
            parse_filename_position(Path::new("/x/Unknown.flac")),
            None
        );
        // "00" — treated as None so it can't hijack
        assert_eq!(parse_filename_position(Path::new("/x/00_Intro.mp3")), None);
    }
}
