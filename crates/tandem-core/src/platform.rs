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
    /// PNG bytes.
    Image(Vec<u8>),
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
    /// Windows switched to a protected screen (an admin prompt, the lock screen) that
    /// no program can type into. Whoever has the keyboard must hand it back.
    InputBlocked,
    /// The user dragged files to a screen edge and the cursor is going over with them.
    DragCrossed {
        edge: Edge,
        frac: f32,
        paths: Vec<std::path::PathBuf>,
    },
    /// The physical left button went up while not swallowing input (the end of a drag
    /// that came back to this computer).
    LocalLeftUp,
    /// A program's window was dragged (Shift held) to a screen edge and the cursor is
    /// going over: open that program on the next computer.
    AppDragCrossed {
        edge: Edge,
        frac: f32,
        app: crate::proto::AppRef,
    },
}

pub trait Platform: Send + Sync + 'static {
    /// Bounding box of the whole desktop the cursor can travel on.
    fn screen(&self) -> Screen;
    /// Edges that lead to a connected peer (one per computer). Empty = no peer: never
    /// capture input.
    fn set_active_edges(&self, edges: &[Edge]);
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
    /// Game mode: no edge switching while a full-screen app is in front.
    fn set_game_mode(&self, _on: bool) {}
    /// Carrying dragged files across the edge (see `CaptureEvent::DragCrossed`).
    fn set_drag_files(&self, _on: bool) {}
    /// The folder a file dropped at this point on screen would land in (an Explorer
    /// window's folder, the desktop), if there is one.
    fn drop_dir_at(&self, _x: i32, _y: i32) -> Option<std::path::PathBuf> {
        None
    }
    /// The files picked up by the drag in progress (read when its button went down), once.
    fn take_drag_files(&self) -> Option<Vec<std::path::PathBuf>> {
        None
    }
    /// Ends the drag in progress here without dropping anything.
    fn cancel_drag(&self) {}
    /// Files are being carried: pushing against an edge with the button down may cross.
    fn set_carrying(&self, _on: bool) {}
    /// Moving programs by dragging their window across the edge (off by default).
    fn set_move_apps(&self, _on: bool) {}
    /// Ask the person at this computer whether to take `app` over from `from`.
    /// Blocks until they answer, so it is only called off the engine's thread.
    fn confirm_move_app(&self, _from: &str, _app: &crate::proto::AppRef) -> bool {
        false
    }
    /// Open `app` here (blocking). Errors are shown to the person on the other computer.
    fn launch_app(&self, _app: &crate::proto::AppRef) -> Result<(), String> {
        Err("Moving apps isn't supported on this computer".into())
    }
    /// Close `app` here (it has opened on the other computer).
    fn close_app(&self, _app: &crate::proto::AppRef) {}
    /// How long the cursor must keep pushing against the edge before it crosses.
    fn set_switch_delay(&self, _ms: u32) {}
    /// Called once at startup; the platform pushes `CaptureEvent`s here.
    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>);
}
