//! Album-cover preview widget.
//!
//! Thin wrapper around [`ratatui_image`] with two roles:
//!
//! 1. Pick the right rendering protocol for the host terminal. Inside a
//!    multiplexer (zellij/tmux) we hard-pin Halfblocks because native
//!    protocols (kitty, sixel, iTerm2) get swallowed by the multiplexer
//!    and leave a blank gap. On a bare terminal we let ratatui-image
//!    query stdio so terminals like ghostty / kitty / iTerm2 get crisp
//!    pixel-perfect rendering.
//! 2. Decode each cover off the render thread. JPEG decode + halfblocks
//!    resize for a multi-megapixel `cover.jpg` can take 1-2 s on a
//!    typical Mac, which would freeze the TUI on first paint of every
//!    new album. We background that decode and show a placeholder until
//!    the protocol is ready, then cache the protocol forever (covers
//!    are tiny — a few hundred KB max — and a session won't open
//!    enough distinct albums for the cache to grow large).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_image::StatefulImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::tui::themes::Theme;

/// Maximum dimension we keep around after decode. Halfblocks' effective
/// resolution is bounded by character cells (~24 cells × 2 = ~48 px tall
/// at our cover-pane size), and even native protocols don't benefit
/// much beyond this for a tile that fills 24 columns. Pre-shrinking
/// makes per-frame resize during scroll cheap and bounds memory use.
const MAX_COVER_DIM: u32 = 512;

/// Session-scoped cover-art renderer.
///
/// Holds one `Picker` chosen at construction time and a per-path map of
/// protocol states (loading, ready, failed). Path is the cache key — two
/// albums sharing a cover file will share the decoded protocol.
pub struct CoverRegistry {
    picker: Picker,
    protocols: HashMap<PathBuf, ProtocolState>,
}

/// Per-cover decode state.
enum ProtocolState {
    /// Background thread is decoding + building the protocol. The
    /// receiver yields exactly one message and is then dropped.
    Loading(mpsc::Receiver<std::result::Result<StatefulProtocol, String>>),
    Ready(StatefulProtocol),
    /// Decode failed — message is shown in the placeholder so the user
    /// has a hint of why nothing's there.
    Failed(String),
}

impl Default for CoverRegistry {
    fn default() -> Self {
        Self {
            picker: pick_picker(),
            protocols: HashMap::new(),
        }
    }
}

impl CoverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the cover at `path` into `area`. Kicks off a background
    /// decode the first time a path is seen; renders a placeholder
    /// while loading, the image once ready, an error message on
    /// failure. The view's render loop continues at full speed —
    /// `try_recv` never blocks.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme, path: &Path) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // First-touch: spawn the decode worker.
        if !self.protocols.contains_key(path) {
            let (tx, rx) = mpsc::channel();
            let path_owned = path.to_path_buf();
            let picker = self.picker.clone();
            std::thread::spawn(move || {
                let _ = tx.send(decode_one(&picker, &path_owned));
            });
            self.protocols
                .insert(path.to_path_buf(), ProtocolState::Loading(rx));
        }

        // Promote Loading → Ready / Failed if the worker has finished.
        if let Some(state) = self.protocols.get_mut(path)
            && let ProtocolState::Loading(rx) = state
            && let Ok(result) = rx.try_recv()
        {
            *state = match result {
                Ok(p) => ProtocolState::Ready(p),
                Err(e) => ProtocolState::Failed(e),
            };
        }

        match self.protocols.get_mut(path) {
            Some(ProtocolState::Ready(protocol)) => {
                let widget = StatefulImage::default();
                frame.render_stateful_widget(widget, area, protocol);
            }
            Some(ProtocolState::Loading(_)) => {
                render_placeholder(frame, area, theme, "loading cover…");
            }
            Some(ProtocolState::Failed(msg)) => {
                let msg = msg.clone();
                render_placeholder(frame, area, theme, &msg);
            }
            None => {}
        }
    }
}

/// Choose the best available protocol for the current terminal.
///
/// Multiplexer detected (`ZELLIJ` / `TMUX`) → hard-pin Halfblocks,
/// because their pass-through of native graphics protocols is unreliable
/// (zellij in particular consistently breaks kitty rendering).
///
/// Otherwise probe via `from_query_stdio` so kitty / sixel / iTerm2
/// terminals get pixel-accurate rendering. The probe has a 2 s internal
/// timeout that falls back to halfblocks; we run it once at startup
/// (called from `App::new`, which runs after raw-mode is enabled and
/// before the event loop starts reading stdin), so the worst-case wait
/// hits at app launch, never on album navigation.
fn pick_picker() -> Picker {
    if std::env::var_os("ZELLIJ").is_some() || std::env::var_os("TMUX").is_some() {
        tracing::debug!("cover picker: multiplexer detected, using halfblocks");
        return Picker::halfblocks();
    }
    match Picker::from_query_stdio() {
        Ok(p) => {
            tracing::debug!(
                "cover picker: from_query_stdio chose {:?}",
                p.protocol_type()
            );
            p
        }
        Err(err) => {
            tracing::debug!("cover picker: from_query_stdio failed ({err}); halfblocks");
            Picker::halfblocks()
        }
    }
}

/// Worker-thread decode + protocol construction. Pre-shrinks oversize
/// originals so the resulting protocol's per-frame resize is cheap.
fn decode_one(picker: &Picker, path: &Path) -> std::result::Result<StatefulProtocol, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| format!("open failed: {}", e))?
        .with_guessed_format()
        .map_err(|e| format!("format probe failed: {}", e))?
        .decode()
        .map_err(|e| format!("decode failed: {}", e))?;

    // `thumbnail` is a fast, aspect-preserving downscale.
    let img = if img.width() > MAX_COVER_DIM || img.height() > MAX_COVER_DIM {
        img.thumbnail(MAX_COVER_DIM, MAX_COVER_DIM)
    } else {
        img
    };

    Ok(picker.new_resize_protocol(img))
}

fn render_placeholder(frame: &mut Frame, area: Rect, theme: &Theme, text: &str) {
    let p = Paragraph::new(Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme.fg_dim),
    )))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border)),
    );
    frame.render_widget(p, area);
}
