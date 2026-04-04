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

pub fn is_confirm(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter
}

pub fn is_save(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s')
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
