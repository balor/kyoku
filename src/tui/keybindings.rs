use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn is_quit(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

pub fn is_help(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('?') || key.code == KeyCode::F(1)
}

pub fn is_search_focus(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('/')
}

pub fn is_back(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
}

pub fn is_tab_switch(key: &KeyEvent) -> bool {
    key.code == KeyCode::Tab
}

pub fn is_up(key: &KeyEvent) -> bool {
    key.code == KeyCode::Up || key.code == KeyCode::Char('k')
}

pub fn is_down(key: &KeyEvent) -> bool {
    key.code == KeyCode::Down || key.code == KeyCode::Char('j')
}

/// Navigation matchers for contexts where a text input is capturing
/// characters: only the arrow keys count, so `j`/`k` insert into the
/// input instead of moving the selection ("Jazz", "junjou" must be
/// typeable in search bars and name fields).
pub fn is_up_arrow(key: &KeyEvent) -> bool {
    key.code == KeyCode::Up
}

pub fn is_down_arrow(key: &KeyEvent) -> bool {
    key.code == KeyCode::Down
}

pub fn is_confirm(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter
}

pub fn is_save(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s')
}

pub fn is_home(key: &KeyEvent) -> bool {
    key.code == KeyCode::Home || key.code == KeyCode::Char('g')
}

pub fn is_end(key: &KeyEvent) -> bool {
    key.code == KeyCode::End || key.code == KeyCode::Char('G')
}

pub fn is_page_up(key: &KeyEvent) -> bool {
    key.code == KeyCode::PageUp
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b'))
}

pub fn is_page_down(key: &KeyEvent) -> bool {
    key.code == KeyCode::PageDown
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f'))
}

pub fn is_half_page_up(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u')
}

pub fn is_half_page_down(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d')
}

pub fn is_refresh(key: &KeyEvent) -> bool {
    key.code == KeyCode::F(5)
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r'))
}

pub fn is_toggle_select(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char(' ')
}

pub fn is_delete(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('d') && key.modifiers.is_empty()
}

/// "C" — fetch cover art from MusicBrainz / Cover Art Archive for the
/// currently focused album. Upper-case so it doesn't collide with `c`
/// bindings in views that browse collections.
pub fn is_fetch_cover(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('C')
}

/// "p" — play the natural unit of the current view (album / track /
/// collection, or the marked selection) in the external music player.
pub fn is_play(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('p') && key.modifiers.is_empty()
}

/// "P" — play the larger scope (whole album / whole collection) from a
/// track-level view. Reserved for "enqueue" in other views later.
pub fn is_play_scope(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('P')
}
