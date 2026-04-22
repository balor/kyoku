//! Album-cover preview widget.
//!
//! Thin wrapper around [`ratatui_image`] with two roles:
//!
//! 1. Pick a *native* rendering protocol (kitty, sixel, iTerm2) for the
//!    host terminal. If only halfblocks is available — or we're inside
//!    a multiplexer that chews up graphics escape sequences — the
//!    picker comes back `None` and callers skip the preview slot
//!    entirely rather than fall back to a pixelated halfblocks render.
//! 2. Decode each cover off the render thread. JPEG decode + resize for
//!    a multi-megapixel `cover.jpg` can take 1-2 s on a typical Mac,
//!    which would freeze the TUI on first paint of every new album. We
//!    background that decode and show a skeleton until the protocol is
//!    ready, then cache the protocol for the lifetime of the session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_image::StatefulImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

use crate::tui::themes::Theme;

/// Maximum dimension we keep around after decode. Native protocols don't
/// benefit much beyond this at our tile size (~30 cells wide). Pre-shrinking
/// makes per-frame resize during scroll cheap and bounds memory use.
const MAX_COVER_DIM: u32 = 512;

/// Session-scoped cover-art renderer.
///
/// Holds an optional `Picker` chosen at construction time (None = no
/// native graphics, callers should skip the preview slot) and a per-path
/// map of protocol states. Path is the cache key — two albums sharing a
/// cover file will share the decoded protocol.
pub struct CoverRegistry {
    picker: Option<Picker>,
    protocols: HashMap<PathBuf, ProtocolState>,
}

/// Per-cover decode state.
enum ProtocolState {
    /// Background thread is decoding + building the protocol. The
    /// receiver yields exactly one message and is then dropped.
    Loading(mpsc::Receiver<std::result::Result<StatefulProtocol, String>>),
    Ready(StatefulProtocol),
    /// Decode failed — logged at transition time. Render draws nothing
    /// for this state; callers that want a layout-level "drop the
    /// preview" can consult [`CoverRegistry::has_failed`] next frame.
    Failed,
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

    /// True when the terminal supports a native graphics protocol. When
    /// false, callers should skip the preview slot in their layout —
    /// there is nothing we'd render here that beats just not showing
    /// a tile at all.
    pub fn can_render_images(&self) -> bool {
        self.picker.is_some()
    }

    /// Has the decode for `path` already failed this session? Callers
    /// use this at layout time to drop the preview slot on subsequent
    /// frames; the very first frame after failure still shows the
    /// skeleton, which is fine.
    pub fn has_failed(&self, path: &Path) -> bool {
        matches!(self.protocols.get(path), Some(ProtocolState::Failed))
    }

    /// Render the cover at `path` into `area`. Assumes `can_render_images()`
    /// is true — callers shouldn't reserve space for a preview when
    /// there's no picker. Kicks off a background decode the first time a
    /// path is seen, draws a skeleton while loading, the image once
    /// ready, and nothing on failure (the caller will stop reserving
    /// space on the next frame).
    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme, path: &Path) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(picker) = self.picker.clone() else {
            return;
        };

        // First-touch: spawn the decode worker.
        if !self.protocols.contains_key(path) {
            let (tx, rx) = mpsc::channel();
            let path_owned = path.to_path_buf();
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
                Err(e) => {
                    tracing::warn!("cover decode failed for {}: {}", path.display(), e);
                    ProtocolState::Failed
                }
            };
        }

        match self.protocols.get_mut(path) {
            Some(ProtocolState::Ready(protocol)) => {
                let widget = StatefulImage::default();
                frame.render_stateful_widget(widget, area, protocol);
            }
            Some(ProtocolState::Loading(_)) => {
                render_skeleton(frame, area, theme);
            }
            Some(ProtocolState::Failed) | None => {}
        }
    }

    /// Row count that makes a `width_cells`-wide box render roughly square
    /// *in pixels* for this picker's cell aspect ratio. Album covers are
    /// overwhelmingly square, so matching the pixel aspect avoids the
    /// "image fills width but leaves a gap at the bottom" artefact on
    /// terminals where cells aren't exactly 1:2. Falls back to 14 when
    /// there's no picker — callers should already be gating on
    /// `can_render_images()` in that case.
    pub fn square_cover_height(&self, width_cells: u16) -> u16 {
        let Some(p) = self.picker.as_ref() else {
            return 14;
        };
        let (cell_w, cell_h) = p.font_size();
        if cell_h == 0 {
            return 14;
        }
        let h = (width_cells as u32).saturating_mul(cell_w as u32) / cell_h as u32;
        h.clamp(6, 32) as u16
    }
}

/// Choose the best available protocol for the current terminal, or
/// `None` when only halfblocks would be available.
///
/// Multiplexer detected (`ZELLIJ` / `TMUX`) → `None`. Their pass-through
/// of native graphics protocols is unreliable (zellij in particular
/// breaks kitty rendering), and halfblocks at our tile size was too
/// pixelated to be worth the attempt.
///
/// Otherwise probe via `from_query_stdio`. A native protocol hit →
/// `Some`; probe failure or halfblocks-only → `None`. The probe has a
/// 2 s internal timeout; we run it once at startup (called from
/// `App::new`, which runs after raw-mode is enabled and before the
/// event loop starts reading stdin), so the worst-case wait hits at
/// app launch, never on album navigation.
fn pick_picker() -> Option<Picker> {
    if std::env::var_os("ZELLIJ").is_some() || std::env::var_os("TMUX").is_some() {
        tracing::debug!("cover picker: multiplexer detected, preview disabled");
        return None;
    }
    match Picker::from_query_stdio() {
        Ok(p) => {
            let proto = p.protocol_type();
            if matches!(proto, ProtocolType::Halfblocks) {
                tracing::debug!("cover picker: only halfblocks available, preview disabled");
                None
            } else {
                tracing::debug!("cover picker: from_query_stdio chose {:?}", proto);
                Some(p)
            }
        }
        Err(err) => {
            tracing::debug!("cover picker: from_query_stdio failed ({err}); preview disabled");
            None
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

/// Loading skeleton — bordered box, single dim label, vertically and
/// horizontally centered. Matches the footprint of a ready image so the
/// layout doesn't shift when decode finishes.
fn render_skeleton(frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let line = Line::from(Span::styled(
        "loading cover…",
        Style::default().fg(theme.fg_dim),
    ))
    .alignment(Alignment::Center);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    frame.render_widget(Paragraph::new(line), v[1]);
}
