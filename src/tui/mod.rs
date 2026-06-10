pub mod app;
pub mod fuzzy;
pub mod keybindings;
pub mod selection;
pub mod themes;
pub mod views;
pub mod widgets;

use std::io;
use std::time::Duration;

use crossterm::event;
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use rusqlite::Connection;

use crate::config::Settings;
use crate::core::importer;
use crate::db::queries;

use self::app::{App, AppAction};
use self::themes::theme_by_name;

pub fn run(conn: Connection, settings: Settings) -> anyhow::Result<()> {
    let theme = theme_by_name(&settings.ui.theme);

    // Calculate inbox count
    let inbox_count = importer::scan_inbox(
        &conn,
        &settings.library.music_dir,
        &settings.library.inbox_dirs,
    )
    .map(|files| files.len())
    .unwrap_or(0);

    // Rebuild FTS index if needed
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks_fts", [], |row| row.get(0))
        .unwrap_or(0);
    let track_count = queries::count_tracks(&conn).unwrap_or(0);
    if fts_count == 0 && track_count > 0 {
        queries::rebuild_fts_index(&conn).ok();
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let mut app = App::new(conn, settings, theme);
    app.inbox_count = inbox_count;

    // Main loop
    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| app.render(frame))?;

        if event::poll(Duration::from_millis(50))? {
            let ev = event::read()?;
            let action = app.handle_event(ev);
            match action {
                AppAction::Quit => break,
                AppAction::None => {}
            }
        }

        app.tick();

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
