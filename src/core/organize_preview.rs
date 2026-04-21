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

use std::collections::BTreeMap;
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
    pub orphans: usize,
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

/// Full per-file listing of the plan.
#[derive(Debug, Clone)]
pub struct DetailPreview {
    pub stats: PlanStats,
    pub moves: Vec<MoveDetail>,
    pub copies: Vec<CopyDetail>,
    pub orphans: Vec<OrphanDetail>,
}

fn stats_from(plan: &OrganizePlan) -> PlanStats {
    PlanStats {
        // Covers count as moves — they're just files the organizer relocates
        // alongside the audio.
        moves_total: plan.moves.len() + plan.cover_moves.len(),
        copies_total: plan.copies.len(),
        skipped: plan.skipped,
        orphans: plan.missing_sources.len(),
    }
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
        stats: stats_from(plan),
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
    // Covers are listed inline with the audio moves — they're files too.
    let moves: Vec<MoveDetail> = plan
        .moves
        .iter()
        .map(|m| (&m.from, &m.to))
        .chain(plan.cover_moves.iter().map(|c| (&c.from, &c.to)))
        .map(|(from, to)| {
            let from_name = name_of(from);
            let to_name = name_of(to);
            let renamed = from_name != to_name;
            MoveDetail {
                from_name,
                from_dir: dir_of(from),
                to_dir: dir_of(to),
                to_name,
                renamed,
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

    DetailPreview {
        stats: stats_from(plan),
        moves,
        copies,
        orphans,
    }
}
