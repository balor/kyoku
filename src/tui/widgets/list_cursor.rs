use crossterm::event::KeyEvent;

use crate::tui::keybindings as keys;

pub const PAGE_SIZE: usize = 20;
pub const HALF_PAGE_SIZE: usize = PAGE_SIZE / 2;

pub fn filtered_indices<T>(
    items: &[T],
    filter: &str,
    mut matches: impl FnMut(&T) -> bool,
) -> Vec<usize> {
    if filter.is_empty() {
        return (0..items.len()).collect();
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches(item))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ListCursor {
    pub selected: usize,
    pub scroll: usize,
}

impl ListCursor {
    pub fn new(selected: usize, scroll: usize) -> Self {
        Self { selected, scroll }
    }

    pub fn clamp(&mut self, count: usize) {
        if count == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    pub fn up(&mut self, count: usize) {
        self.clamp(count);
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self, count: usize) {
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
        } else {
            self.selected = 0;
        }
    }

    pub fn page_up(&mut self, count: usize) {
        self.clamp(count);
        self.selected = self.selected.saturating_sub(PAGE_SIZE);
    }

    pub fn page_down(&mut self, count: usize) {
        if count > 0 {
            self.selected = (self.selected + PAGE_SIZE).min(count - 1);
        } else {
            self.selected = 0;
        }
    }

    pub fn half_page_up(&mut self, count: usize) {
        self.clamp(count);
        self.selected = self.selected.saturating_sub(HALF_PAGE_SIZE);
    }

    pub fn half_page_down(&mut self, count: usize) {
        if count > 0 {
            self.selected = (self.selected + HALF_PAGE_SIZE).min(count - 1);
        } else {
            self.selected = 0;
        }
    }

    pub fn move_top(&mut self, _count: usize) {
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_bottom(&mut self, count: usize) {
        self.selected = count.saturating_sub(1);
    }

    /// Apply standard list navigation. Returns true if the key was consumed.
    pub fn handle_key(&mut self, key: &KeyEvent, count: usize) -> bool {
        if keys::is_up(key) {
            self.up(count);
        } else if keys::is_down(key) {
            self.down(count);
        } else if keys::is_page_up(key) {
            self.page_up(count);
        } else if keys::is_page_down(key) {
            self.page_down(count);
        } else if keys::is_half_page_up(key) {
            self.half_page_up(count);
        } else if keys::is_half_page_down(key) {
            self.half_page_down(count);
        } else if keys::is_home(key) {
            self.move_top(count);
        } else if keys::is_end(key) {
            self.move_bottom(count);
        } else {
            return false;
        }
        true
    }

    /// Apply navigation in a live text-input context where `j`/`k` must stay
    /// typeable and only arrow keys move the list.
    pub fn handle_text_input_key(&mut self, key: &KeyEvent, count: usize) -> bool {
        if keys::is_up_arrow(key) {
            self.up(count);
        } else if keys::is_down_arrow(key) {
            self.down(count);
        } else if keys::is_page_up(key) {
            self.page_up(count);
        } else if keys::is_page_down(key) {
            self.page_down(count);
        } else if keys::is_half_page_up(key) {
            self.half_page_up(count);
        } else if keys::is_half_page_down(key) {
            self.half_page_down(count);
        } else if keys::is_home(key) {
            self.move_top(count);
        } else if keys::is_end(key) {
            self.move_bottom(count);
        } else {
            return false;
        }
        true
    }
}
