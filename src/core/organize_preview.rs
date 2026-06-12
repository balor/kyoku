//! Renderer-neutral preview of an [`OrganizePlan`].
//!
//! Both the CLI and the TUI render organize plans in two modes — a grouped
//! summary and a per-file detail listing. The two frontends use different
//! output media (styled ratatui lines vs plain stdout strings) but the
//! underlying structure is identical, so we collect it once here. Each
//! frontend only has to iterate these structs and format them.
//!
//! Keep this module free of presentation concerns (no styling, no padding,
//! no truncation). Those decisions belong to the renderer.
//!
//! The preview is cheap to build; callers should re-derive it from the plan
//! rather than cache it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::core::organizer::OrganizePlan;

/// One group of moves sharing a `(from_dir, to_dir)` pair.
#[derive(Debug, Clone)]
pub struct DirMoveGroup {
    pub from_dir: String,
    pub to_dir: String,
    pub count: usize,
}

/// One group of moves whose `from_dir == to_dir` — a rename in place.
#[derive(Debug, Clone)]
pub struct InPlaceRenameGroup {
    pub dir: String,
    pub count: usize,
}

/// One group of copies going into the same collection.
#[derive(Debug, Clone)]
pub struct CollectionCopyGroup {
    pub collection_name: String,
    pub count: usize,
}

/// Aggregated counts shared between summary and detail views.
/// `moves_total` includes cover-art moves — covers are just files like the
/// audio, and the UI treats them uniformly.
#[derive(Debug, Clone, Copy)]
pub struct PlanStats {
    pub moves_total: usize,
    pub copies_total: usize,
    pub skipped: usize,
    /// DB rows pointing at missing files — pruned during apply.
    pub orphans: usize,
    /// Files on disk without a DB row that will be deleted as standalone
    /// unlinks (not overwritten by any move). The user sees these as
    /// "purely dangling" — the ones that cease to exist entirely.
    pub file_orphans: usize,
    /// Orphans whose paths coincide with a move destination; counted
    /// separately to avoid double-warning the user (the overwrite is
    /// surfaced inline on the corresponding move line).
    pub file_orphans_absorbed: usize,
}

/// Grouped overview of a plan — moves collapsed by directory pair, copies
/// collapsed by collection.
#[derive(Debug, Clone)]
pub struct SummaryPreview {
    pub stats: PlanStats,
    pub dir_moves: Vec<DirMoveGroup>,
    pub in_place_renames: Vec<InPlaceRenameGroup>,
    pub collection_copies: Vec<CollectionCopyGroup>,
}

/// Per-file move entry for the detail view.
#[derive(Debug, Clone)]
pub struct MoveDetail {
    pub from_name: String,
    pub from_dir: String,
    pub to_dir: String,
    pub to_name: String,
    /// `from_name != to_name` — renderers may highlight the renamed filename.
    pub renamed: bool,
    /// True when this move's destination path matches a pending
    /// `file_orphans` entry. Typical flow: a dup "Keep New" import
    /// wrote the old library file to `orphaned_files` for cleanup, and
    /// the new import's organize target resolves to the same path.
    /// Rendered as an inline "will overwrite existing file" note.
    pub overwrites_orphan: bool,
}

/// Per-file copy entry for the detail view.
#[derive(Debug, Clone)]
pub struct CopyDetail {
    pub name: String,
    pub to_dir: String,
    pub collection_name: String,
}

/// Per-file orphaned-source entry for the detail view.
#[derive(Debug, Clone)]
pub struct OrphanDetail {
    pub id: i64,
    pub title: String,
    pub path: PathBuf,
}

/// Per-file orphan file entry — file on disk whose DB row is gone.
/// `label` is a pre-formatted human-readable name for rendering (title
/// if we have it, else the filename).
#[derive(Debug, Clone)]
pub struct FileOrphanDetail {
    pub path: PathBuf,
    pub label: String,
    pub reason: String,
}

/// Full per-file listing of the plan.
#[derive(Debug, Clone)]
pub struct DetailPreview {
    pub stats: PlanStats,
    pub moves: Vec<MoveDetail>,
    pub copies: Vec<CopyDetail>,
    pub orphans: Vec<OrphanDetail>,
    pub file_orphans: Vec<FileOrphanDetail>,
}

fn stats_from(plan: &OrganizePlan, dangling_orphans: usize, absorbed_orphans: usize) -> PlanStats {
    PlanStats {
        // Covers count as moves — they're just files the organizer relocates
        // alongside the audio.
        moves_total: plan.moves.len() + plan.cover_moves.len(),
        copies_total: plan.copies.len(),
        skipped: plan.skipped,
        orphans: plan.missing_sources.len(),
        file_orphans: dangling_orphans,
        file_orphans_absorbed: absorbed_orphans,
    }
}

/// Insert both the literal and canonicalized form of `p` into `set`.
/// Canonicalization catches macOS NFC/NFD and symlink variants when the
/// file exists; if it doesn't, the literal form still participates.
fn insert_path_forms(p: &Path, set: &mut HashSet<String>) {
    set.insert(p.display().to_string());
    if let Ok(canon) = std::fs::canonicalize(p) {
        set.insert(canon.display().to_string());
    }
}

/// True when any form of `p` (literal or canonical) is present in `set`.
fn path_in_set(p: &Path, set: &HashSet<String>) -> bool {
    if set.contains(&p.display().to_string()) {
        return true;
    }
    if let Ok(canon) = std::fs::canonicalize(p)
        && set.contains(&canon.display().to_string())
    {
        return true;
    }
    false
}

/// Classify file_orphans against move/copy destinations.
/// Returns the set of destination paths (literal + canonical) and the
/// set of orphan paths (literal + canonical). Both are pre-expanded so
/// caller-side `contains` checks remain O(1).
fn overlap_sets(plan: &OrganizePlan) -> (HashSet<String>, HashSet<String>) {
    let mut dests: HashSet<String> = HashSet::new();
    for m in &plan.moves {
        insert_path_forms(&m.to, &mut dests);
    }
    for cm in &plan.cover_moves {
        insert_path_forms(&cm.to, &mut dests);
    }
    for c in &plan.copies {
        insert_path_forms(&c.to, &mut dests);
    }

    let mut orphs: HashSet<String> = HashSet::new();
    for e in &plan.file_orphans {
        insert_path_forms(&e.path, &mut orphs);
    }

    (dests, orphs)
}

fn dir_of(p: &Path) -> String {
    p.parent().map(|d| d.display().to_string()).unwrap_or_default()
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Build a grouped summary: moves collapsed by `(from_dir → to_dir)`, with
/// in-place renames (`from_dir == to_dir`) split out so they don't render as
/// confusing identical-path transitions; copies collapsed by collection.
pub fn build_summary(plan: &OrganizePlan) -> SummaryPreview {
    let (dests, _orphs) = overlap_sets(plan);
    // Partition the orphan list: absorbed ones are folded into their
    // corresponding moves in the renderer; only truly dangling orphans
    // surface in the dedicated "will be deleted" section.
    let mut dangling = 0usize;
    let mut absorbed = 0usize;
    for e in &plan.file_orphans {
        if path_in_set(&e.path, &dests) {
            absorbed += 1;
        } else {
            dangling += 1;
        }
    }
    let mut dir_groups: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut rename_groups: BTreeMap<String, usize> = BTreeMap::new();
    // Treat audio moves and cover moves uniformly — both are file relocations
    // keyed by their (from_dir → to_dir) pair. This is what lets a per-album
    // move of "track1.flac + track2.flac + cover.jpg" collapse into a single
    // "3× /src → /dst" group in the summary.
    let move_pairs = plan
        .moves
        .iter()
        .map(|m| (&m.from, &m.to))
        .chain(plan.cover_moves.iter().map(|c| (&c.from, &c.to)));
    for (from, to) in move_pairs {
        let from_dir = dir_of(from);
        let to_dir = dir_of(to);
        if from_dir == to_dir {
            *rename_groups.entry(from_dir).or_insert(0) += 1;
        } else {
            *dir_groups.entry((from_dir, to_dir)).or_insert(0) += 1;
        }
    }

    let mut coll_groups: BTreeMap<String, usize> = BTreeMap::new();
    for c in &plan.copies {
        *coll_groups.entry(c.collection_name.clone()).or_insert(0) += 1;
    }

    SummaryPreview {
        stats: stats_from(plan, dangling, absorbed),
        dir_moves: dir_groups
            .into_iter()
            .map(|((from_dir, to_dir), count)| DirMoveGroup {
                from_dir,
                to_dir,
                count,
            })
            .collect(),
        in_place_renames: rename_groups
            .into_iter()
            .map(|(dir, count)| InPlaceRenameGroup { dir, count })
            .collect(),
        collection_copies: coll_groups
            .into_iter()
            .map(|(collection_name, count)| CollectionCopyGroup {
                collection_name,
                count,
            })
            .collect(),
    }
}

/// Build a per-file listing in the plan's own order.
pub fn build_details(plan: &OrganizePlan) -> DetailPreview {
    let (dests, orphs) = overlap_sets(plan);

    // Covers are listed inline with the audio moves — they're files too.
    // `overwrites_orphan` is filled in by checking whether the move
    // destination matches any pending orphan path, so the renderer can
    // tag the line as "will overwrite existing file" instead of the user
    // seeing both a move and a separate orphan-delete for the same path.
    let moves: Vec<MoveDetail> = plan
        .moves
        .iter()
        .map(|m| (&m.from, &m.to))
        .chain(plan.cover_moves.iter().map(|c| (&c.from, &c.to)))
        .map(|(from, to)| {
            let from_name = name_of(from);
            let to_name = name_of(to);
            let renamed = from_name != to_name;
            let overwrites_orphan = path_in_set(to, &orphs);
            MoveDetail {
                from_name,
                from_dir: dir_of(from),
                to_dir: dir_of(to),
                to_name,
                renamed,
                overwrites_orphan,
            }
        })
        .collect();

    let copies = plan
        .copies
        .iter()
        .map(|c| CopyDetail {
            name: name_of(&c.to),
            to_dir: dir_of(&c.to),
            collection_name: c.collection_name.clone(),
        })
        .collect();

    let orphans = plan
        .missing_sources
        .iter()
        .map(|(id, path, title)| OrphanDetail {
            id: *id,
            title: title.clone(),
            path: path.clone(),
        })
        .collect();

    // Only surface "purely dangling" orphans here — ones that don't share
    // a path with any move destination. The absorbed ones are represented
    // inline on the move lines via `overwrites_orphan`, so the user
    // doesn't see a confusing pair of "will move X there" + "will delete X".
    let mut absorbed_count = 0usize;
    let file_orphans: Vec<FileOrphanDetail> = plan
        .file_orphans
        .iter()
        .filter(|e| {
            let absorbed = path_in_set(&e.path, &dests);
            if absorbed {
                absorbed_count += 1;
            }
            !absorbed
        })
        .map(|e| {
            // Prefer the tag snapshot when we have one; fall back to the
            // filename so the user always sees *something* identifiable.
            // Album is appended when available — helps distinguish same-title
            // tracks from different releases in the orphan list.
            let base = match (e.title.as_deref(), e.artist.as_deref()) {
                (Some(t), Some(a)) => format!("{} — {}", a, t),
                (Some(t), None) => t.to_string(),
                (None, _) => e
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
            };
            let label = match e.album_title.as_deref() {
                Some(alb) if !alb.is_empty() => format!("{} · {}", base, alb),
                _ => base,
            };
            FileOrphanDetail {
                path: e.path.clone(),
                label,
                reason: e.reason.clone(),
            }
        })
        .collect();

    DetailPreview {
        stats: stats_from(plan, file_orphans.len(), absorbed_count),
        moves,
        copies,
        orphans,
        file_orphans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::organizer::{FileMove, FileOrphanEntry, OrganizePlan};

    fn empty_plan() -> OrganizePlan {
        OrganizePlan::default()
    }

    fn move_entry(from: &str, to: &str) -> FileMove {
        FileMove {
            track_id: 1,
            from: PathBuf::from(from),
            to: PathBuf::from(to),
            also_collection: None,
        }
    }

    fn orphan_entry(id: i64, path: &str) -> FileOrphanEntry {
        FileOrphanEntry {
            id,
            path: PathBuf::from(path),
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            album_title: Some("Album".into()),
            reason: "replaced by duplicate during import".into(),
        }
    }

    #[test]
    fn details_tag_move_when_destination_matches_orphan_and_hide_it_from_orphan_list() {
        let mut plan = empty_plan();
        let overlap = "/music/Artist/Album (2024)/03 Song.mp3";
        let dangling = "/music/Artist/Album (2024)/old_only.mp3";
        plan.moves.push(move_entry("/inbox/03 Song.mp3", overlap));
        plan.file_orphans.push(orphan_entry(1, overlap));
        plan.file_orphans.push(orphan_entry(2, dangling));

        let preview = build_details(&plan);

        // The single move is tagged as an overwrite.
        assert_eq!(preview.moves.len(), 1);
        assert!(
            preview.moves[0].overwrites_orphan,
            "move whose destination matches an orphan path must be tagged"
        );

        // Absorbed orphan is hidden from the orphan list; dangling one stays.
        assert_eq!(preview.file_orphans.len(), 1);
        assert_eq!(
            preview.file_orphans[0].path,
            PathBuf::from(dangling),
            "only the orphan without a matching move should remain"
        );
        assert_eq!(preview.stats.file_orphans, 1, "dangling count");
        assert_eq!(preview.stats.file_orphans_absorbed, 1, "absorbed count");
    }

    #[test]
    fn details_all_orphans_dangling_when_no_move_overlaps() {
        let mut plan = empty_plan();
        plan.moves.push(move_entry(
            "/inbox/a.mp3",
            "/music/Artist/Album/new.mp3",
        ));
        plan.file_orphans.push(orphan_entry(1, "/music/other/leftover.mp3"));

        let preview = build_details(&plan);

        assert!(!preview.moves[0].overwrites_orphan);
        assert_eq!(preview.file_orphans.len(), 1);
        assert_eq!(preview.stats.file_orphans, 1);
        assert_eq!(preview.stats.file_orphans_absorbed, 0);
    }

    #[test]
    fn summary_stats_split_matches_details_split() {
        // Summary and details must agree on the dangling vs absorbed counts
        // — otherwise the footer and body would contradict each other.
        let mut plan = empty_plan();
        let overlap = "/music/Artist/Album/01.mp3";
        plan.moves.push(move_entry("/inbox/01.mp3", overlap));
        plan.file_orphans.push(orphan_entry(1, overlap));
        plan.file_orphans.push(orphan_entry(2, "/music/dangler.mp3"));

        let details = build_details(&plan);
        let summary = build_summary(&plan);

        assert_eq!(summary.stats.file_orphans, details.stats.file_orphans);
        assert_eq!(
            summary.stats.file_orphans_absorbed,
            details.stats.file_orphans_absorbed
        );
    }
}
