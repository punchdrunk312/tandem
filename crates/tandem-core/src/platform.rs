//! Everything OS-specific lives behind this trait so the engine can be
//! tested on any machine with `mock::MockPlatform`.

use crate::proto::{Edge, InputEvent};
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Top-left of the (virtual) desktop; negative with monitors left of/above the primary.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Screen {
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipContent {
    Text(String),
    Files(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureEvent {
    /// The local cursor pushed through `edge` at `frac` (0..1) along it.
    /// The platform has already started swallowing local input.
    Crossed { edge: Edge, frac: f32 },
    /// A physical input event, delivered only while swallowing.
    Input(InputEvent),
    /// Emergency hotkey pressed while swallowing; swallowing is already off.
    Failsafe,
    /// Hotkey pressed while not swallowing: the user wants to jump to the peer.
    HotkeyEngage,
    /// The system clipboard changed.
    ClipboardChanged,
}

pub trait Platform: Send + Sync + 'static {
    /// Bounding box of the whole desktop the cursor can travel on.
    fn screen(&self) -> Screen;
    /// Edge that leads to the peer. `None` = no peer: never capture input.
    fn set_active_edge(&self, edge: Option<Edge>);
    /// Start swallowing local input, parking the cursor mid-screen (hotkey jump).
    fn begin_capture(&self);
    /// Stop swallowing local input and place the cursor.
    fn end_capture(&self, x: i32, y: i32);
    fn inject(&self, ev: &InputEvent);
    fn cursor_pos(&self) -> (i32, i32);
    fn set_cursor(&self, x: i32, y: i32);
    /// Release every key/button we have injected and not yet released.
    fn release_all(&self);
    fn clipboard_get(&self) -> Option<ClipContent>;
    fn clipboard_set(&self, c: &ClipContent);
    /// Called once at startup; the platform pushes `CaptureEvent`s here.
    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>);
}
