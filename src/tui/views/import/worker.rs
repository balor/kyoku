//! Background import worker. Runs on its own thread with its own DB
//! connection so we don't block the TUI's main-thread `Connection`.

use std::sync::mpsc;

use crate::config::settings::NameScriptPreference;
use crate::core::importer::detect_sibling_cover;
use crate::core::tagger;
use crate::db::queries;
use crate::external::musicbrainz::MbClient;

use super::{GroupAction, ImportGroup, ImportMessage};

pub(super) fn run_import_worker(
    groups_to_import: Vec<ImportGroup>,
    user_skipped: u32,
    rate_limit_ms: u64,
    name_script: NameScriptPreference,
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

    // Fetch full release data for MB-matched groups (search results don't
    // include track listings — we need them for per-track metadata).
    let mut mb_client = MbClient::new(rate_limit_ms, name_script);

    {
        let _ = tx.send(ImportMessage::Progress(done, total_tracks));
        for group in &groups_to_import {
            let loose = group.action == GroupAction::Loose;

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
                    None,
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

            for (i, (track, _tag_data)) in group.tracks.iter().enumerate() {
                let path_str = track.file_path.display().to_string();

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
                            // Match local track to MB track by position (1-based)
                            let mb_track = mb.tracks.iter().find(|t| t.position == (i + 1) as u32);
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
        parts.push(format!("Duplicates: {}", skipped));
    }
    if user_skipped > 0 {
        parts.push(format!("Skipped: {}", user_skipped));
    }
    if errors > 0 {
        parts.push(format!("Errors: {}", errors));
    }
    let _ = tx.send(ImportMessage::Complete(parts.join(", ")));
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
