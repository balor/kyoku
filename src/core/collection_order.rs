//! Shared ordering policy for tracks inside collections.
//!
//! Collections have their own durable order (`collection_tracks.position`) that
//! is independent of album track numbers. For legacy rows that predate stored
//! positions, we infer a sensible order: coherent disc/track metadata first,
//! otherwise the caller-provided add/import order. Title is never the primary
//! fallback because alphabetical collection order is almost never user intent.

use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct CollectionOrderItem {
    /// Index into the caller's parallel row/vector storage.
    pub index: usize,
    /// Stable DB id or caller-provided id. Used only as a final tie-breaker.
    pub track_id: i64,
    /// Durable collection position, when set.
    pub explicit_position: Option<u32>,
    pub disc_number: u32,
    pub track_number: Option<u32>,
    /// DB album id. If every item has the same album id, track metadata is
    /// considered cohesive even for partial albums (e.g. tracks 5-11).
    pub album_id: Option<i64>,
    /// Import-time album key for items that are not in the DB yet.
    pub album_key: Option<String>,
    /// Caller-provided add/import order. For DB rows this should be
    /// `(added_at, track_id)` order; for scans it is scan order.
    pub added_order: usize,
    pub title: String,
}

/// Return caller indices in collection display / filesystem order.
pub fn ordered_indices(items: &[CollectionOrderItem]) -> Vec<usize> {
    let positions = effective_position_map(items);
    let fallback = fallback_rank_map(items);

    let mut refs: Vec<&CollectionOrderItem> = items.iter().collect();
    refs.sort_by(|a, b| {
        positions
            .get(&a.index)
            .copied()
            .unwrap_or(u32::MAX)
            .cmp(&positions.get(&b.index).copied().unwrap_or(u32::MAX))
            // Explicit positions are the source of truth when present; this
            // tie-break only matters for malformed/partial legacy data.
            .then_with(|| {
                a.explicit_position
                    .is_none()
                    .cmp(&b.explicit_position.is_none())
            })
            .then_with(|| {
                fallback
                    .get(&a.index)
                    .copied()
                    .unwrap_or(usize::MAX)
                    .cmp(&fallback.get(&b.index).copied().unwrap_or(usize::MAX))
            })
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| a.track_id.cmp(&b.track_id))
    });
    refs.into_iter().map(|item| item.index).collect()
}

/// Return `(caller_index, effective_position)` pairs.
///
/// Explicit positions are returned as-is. Unpositioned rows receive the
/// 1-based rank they would have under the legacy fallback policy.
pub fn effective_positions(items: &[CollectionOrderItem]) -> Vec<(usize, u32)> {
    let positions = effective_position_map(items);
    items
        .iter()
        .map(|item| (item.index, positions.get(&item.index).copied().unwrap_or(0)))
        .collect()
}

fn effective_position_map(items: &[CollectionOrderItem]) -> HashMap<usize, u32> {
    let fallback = fallback_rank_map(items);
    let mut out = HashMap::new();
    for item in items {
        let pos = item
            .explicit_position
            .filter(|p| *p > 0)
            .unwrap_or_else(|| fallback.get(&item.index).copied().unwrap_or(usize::MAX) as u32);
        out.insert(item.index, pos);
    }
    out
}

fn fallback_rank_map(items: &[CollectionOrderItem]) -> HashMap<usize, usize> {
    let mut ordered: Vec<&CollectionOrderItem> = items.iter().collect();
    if has_cohesive_metadata(items) {
        ordered.sort_by(|a, b| {
            a.disc_number
                .cmp(&b.disc_number)
                .then_with(|| {
                    a.track_number
                        .unwrap_or(0)
                        .cmp(&b.track_number.unwrap_or(0))
                })
                .then_with(|| a.added_order.cmp(&b.added_order))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| a.track_id.cmp(&b.track_id))
        });
    } else {
        ordered.sort_by(|a, b| {
            a.added_order
                .cmp(&b.added_order)
                .then_with(|| a.track_id.cmp(&b.track_id))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
    }

    let mut ranks = HashMap::new();
    for (rank, item) in ordered.into_iter().enumerate() {
        ranks.insert(item.index, rank + 1);
    }
    ranks
}

fn has_cohesive_metadata(items: &[CollectionOrderItem]) -> bool {
    if items.is_empty() {
        return false;
    }

    let mut slots = HashSet::new();
    for item in items {
        let Some(track) = item.track_number.filter(|n| *n > 0) else {
            return false;
        };
        let disc = item.disc_number.max(1);
        if !slots.insert((disc, track)) {
            return false;
        }
    }

    if let Some(first) = items[0].album_id
        && items.iter().all(|item| item.album_id == Some(first))
    {
        return true;
    }

    if let Some(first) = items[0].album_key.as_deref()
        && !first.is_empty()
        && items
            .iter()
            .all(|item| item.album_key.as_deref() == Some(first))
    {
        return true;
    }

    // If any album identity is present but the rows are not all the same
    // album, the numbers are likely inherited from unrelated source albums.
    // Dense 1..N across mixed albums is not enough evidence of collection
    // intent; preserve add/import order instead.
    if items.iter().any(|item| item.album_id.is_some())
        || items
            .iter()
            .any(|item| item.album_key.as_deref().is_some_and(|s| !s.is_empty()))
    {
        return false;
    }

    // Album-less collection metadata is only considered cohesive when each
    // disc has a dense 1..N track sequence. Random inherited album numbers
    // like 2, 7, 13 (or duplicates, handled above) fall back to add order.
    let mut by_disc: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for item in items {
        by_disc
            .entry(item.disc_number.max(1))
            .or_default()
            .push(item.track_number.unwrap_or(0));
    }
    for nums in by_disc.values_mut() {
        nums.sort_unstable();
        for (idx, n) in nums.iter().enumerate() {
            if *n != (idx as u32 + 1) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        index: usize,
        pos: Option<u32>,
        album_id: Option<i64>,
        track: Option<u32>,
    ) -> CollectionOrderItem {
        CollectionOrderItem {
            index,
            track_id: index as i64 + 10,
            explicit_position: pos,
            disc_number: 1,
            track_number: track,
            album_id,
            album_key: None,
            added_order: index,
            title: format!("t{index}"),
        }
    }

    #[test]
    fn explicit_positions_win() {
        let items = vec![
            item(0, Some(3), None, Some(1)),
            item(1, Some(1), None, Some(2)),
        ];
        assert_eq!(ordered_indices(&items), vec![1, 0]);
    }

    #[test]
    fn same_album_uses_track_numbers_even_for_partial_album() {
        let items = vec![
            item(0, None, Some(7), Some(9)),
            item(1, None, Some(7), Some(5)),
        ];
        assert_eq!(ordered_indices(&items), vec![1, 0]);
    }

    #[test]
    fn dense_albumless_metadata_uses_track_numbers() {
        let items = vec![item(0, None, None, Some(2)), item(1, None, None, Some(1))];
        assert_eq!(ordered_indices(&items), vec![1, 0]);
    }

    #[test]
    fn dense_mixed_album_metadata_uses_added_order() {
        let items = vec![
            item(0, None, Some(10), Some(2)),
            item(1, None, Some(11), Some(1)),
        ];
        assert_eq!(ordered_indices(&items), vec![0, 1]);
    }

    #[test]
    fn scrambled_albumless_metadata_uses_added_order() {
        let items = vec![item(0, None, None, Some(7)), item(1, None, None, Some(2))];
        assert_eq!(ordered_indices(&items), vec![0, 1]);
    }

    #[test]
    fn duplicate_or_missing_track_numbers_use_added_order() {
        let dupes = vec![item(0, None, None, Some(1)), item(1, None, None, Some(1))];
        assert_eq!(ordered_indices(&dupes), vec![0, 1]);

        let missing = vec![item(0, None, None, None), item(1, None, None, Some(1))];
        assert_eq!(ordered_indices(&missing), vec![0, 1]);
    }
}
