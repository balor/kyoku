use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::themes::Theme;

/// How long a cached existence check stays valid. The render loop ticks
/// ~20x/sec; without this, every frame would `stat()` the same path.
const CHECK_INTERVAL: Duration = Duration::from_secs(1);

thread_local! {
    /// At most one cached result — `(path, was_missing, checked_at)`.
    /// Overwritten whenever the path changes or the TTL expires, so this
    /// stays O(1) regardless of how many tracks the user scrolls through.
    static LAST_CHECK: RefCell<Option<(String, bool, Instant)>> =
        const { RefCell::new(None) };
}

/// Render a one-line footer showing a file path, prefixed with a yellow ⚠
/// when the path is non-empty and does not exist on disk. The existence
/// check is throttled to once per second per path.
pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, path: &str) {
    let missing = is_missing(path);
    let mut spans = Vec::with_capacity(2);
    if missing {
        spans.push(Span::styled(
            " ⚠",
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let path_style = if missing {
        Style::default().fg(theme.yellow)
    } else {
        Style::default().fg(theme.fg_muted)
    };
    spans.push(Span::styled(format!(" {}", path), path_style));
    let p = Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg_alt));
    frame.render_widget(p, area);
}

fn is_missing(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let now = Instant::now();
    LAST_CHECK.with(|cell| {
        let mut cached = cell.borrow_mut();
        if let Some((cached_path, cached_missing, checked_at)) = cached.as_ref()
            && cached_path == path
            && now.duration_since(*checked_at) < CHECK_INTERVAL
        {
            return *cached_missing;
        }
        let result = !Path::new(path).exists();
        *cached = Some((path.to_string(), result, now));
        result
    })
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::tui::themes::TOKYO_NIGHT;

    fn render_buffer(path: &str) -> Buffer {
        let width = (path.chars().count() + 4).max(8) as u16;
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &TOKYO_NIGHT, path))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_line(buffer: &Buffer) -> String {
        let mut out = String::new();
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, 0)].symbol());
        }
        out.trim_end().to_string()
    }

    #[test]
    fn existing_path_renders_without_warning() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let buffer = render_buffer(&path);

        assert_eq!(buffer_line(&buffer), format!(" {path}"));
        assert_eq!(buffer[(0, 0)].fg, TOKYO_NIGHT.fg_muted);
        assert_eq!(buffer[(1, 0)].fg, TOKYO_NIGHT.fg_muted);
    }

    #[test]
    fn missing_path_renders_warning_and_yellow_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing.flac");
        let path = path.to_string_lossy().to_string();

        let buffer = render_buffer(&path);

        assert_eq!(buffer_line(&buffer), format!(" ⚠ {path}"));
        assert_eq!(buffer[(1, 0)].symbol(), "⚠");
        assert_eq!(buffer[(1, 0)].fg, TOKYO_NIGHT.yellow);
        assert!(buffer[(1, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(3, 0)].fg, TOKYO_NIGHT.yellow);
        assert!(!buffer[(3, 0)].modifier.contains(Modifier::BOLD));
    }
}
