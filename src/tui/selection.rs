//! Multi-select helper shared across list-oriented views.
//!
//! Selection is keyed by **row id (i64)**, not visual index, so filtering or
//! re-sorting the underlying list doesn't lose the user's marks. Each view
//! that wants multi-select grows a `selection: Selection` field and renders
//! a gutter marker (e.g. `▎`) on rows whose id is contained in the set.

use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct Selection {
    marked: HashSet<i64>,
}

impl Selection {
    pub fn toggle(&mut self, id: i64) {
        if !self.marked.insert(id) {
            self.marked.remove(&id);
        }
    }

    pub fn clear(&mut self) {
        self.marked.clear();
    }

    pub fn contains(&self, id: i64) -> bool {
        self.marked.contains(&id)
    }

    /// Return the marked ids in ascending order — stable for test assertions
    /// and for "first row wins" policies at call sites.
    pub fn ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.marked.iter().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn is_empty(&self) -> bool {
        self.marked.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_then_removes() {
        let mut s = Selection::default();
        assert!(s.is_empty());
        s.toggle(7);
        assert!(s.contains(7));
        assert_eq!(s.ids(), vec![7]);
        s.toggle(7);
        assert!(!s.contains(7));
        assert!(s.is_empty());
    }

    #[test]
    fn ids_are_sorted() {
        let mut s = Selection::default();
        s.toggle(5);
        s.toggle(1);
        s.toggle(3);
        assert_eq!(s.ids(), vec![1, 3, 5]);
    }

    #[test]
    fn clear_resets() {
        let mut s = Selection::default();
        s.toggle(1);
        s.toggle(2);
        s.clear();
        assert!(s.is_empty());
    }
}
