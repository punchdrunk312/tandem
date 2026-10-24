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
    /// A program's window (or a browser tab) was dragged to a connected edge and held
    /// there: offer to move that program to the computer on that side. The platform has
    /// already put the dragged window back; the cursor stays on this computer.
    AppDragged {
        edge: Edge,
        app: crate::proto::AppRef,
    },
    /// A window being dragged (any program's) has started to go past a connected edge:
    /// the computer on that side shows the part that is over. `w` x `h` is its size.
    SpanBegin { edge: Edge, w: u32, h: u32 },
    /// The window moved (see `proto::Msg::SpanPos`).
    SpanMove {
        crossed: i32,
        along: f32,
        gx: i32,
        gy: i32,
    },
    /// A fresh picture of the window, as JPEG.
    SpanFrame(Vec<u8>),
    /// The drag across ended. `moved`: let go more than halfway over, so the program
    /// (`app`, if it is one that can be moved) goes to the other computer and the mouse
    /// carries on there at `frac` along the edge. When this computer's own mouse did it,
    /// the platform also sends `Crossed`. Otherwise the window stays here.
    SpanEnd {
        moved: bool,
        app: Option<crate::proto::AppRef>,
        frac: f32,
    },
    /// Second-monitor mode, the computer showing a mirror: the pointer went onto mirror
    /// `id` at (`rx`, `ry`) inside it. The platform has already started swallowing input
    /// (the owner of the window takes the pointer).
    PortalIn { id: u64, rx: i32, ry: i32 },
    /// The computer that owns the window: the pointer left the window it was on, at
    /// (`rx`, `ry`) relative to it. The platform has already started swallowing input
    /// (the pointer carries on over the mirror).
    PortalOut { rx: i32, ry: i32 },
    /// Where the pointer is inside the window it is on (the owner, while it has it).
    PortalCursor { id: u64, rx: i32, ry: i32, shape: u8 },
    /// A fresh picture of a mirrored window of ours (`w` x `h`, JPEG).
    MirrorFrame {
        id: u64,
        w: u32,
        h: u32,
        jpeg: Vec<u8>,
    },
    /// A mirrored window of ours was dragged by its title bar (see `Msg::MirrorMoved`).
    MirrorMoved {
        id: u64,
        dx: i32,
        dy: i32,
        done: bool,
    },
    /// A mirrored window of ours was closed.
    MirrorGone { id: u64 },
    /// The person closed the mirror of another computer's window shown here.
    MirrorDismissed { id: u64 },
    /// The charger was plugged in or pulled out just now: read how the computer is doing
    /// at once (the bar says so within a moment, like the system itself).
    PowerChanged,
}

/// A window being dragged over from another computer, as this computer shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpanView {
    /// The edge of this screen the window comes in through.
    pub entry: Edge,
    pub crossed: i32,
    pub along: f32,
    pub w: u32,
    pub h: u32,
    pub gx: i32,
    pub gy: i32,
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
    /// Ask the person here whether to move `app` over to the computer called `to`.
    /// Blocks until they answer, so it is only called off the engine's thread.
    fn confirm_move_app(&self, _to: &str, _app: &crate::proto::AppRef) -> bool {
        false
    }
    /// Open `app` here (blocking). Errors are shown to the person on the other computer.
    fn launch_app(&self, _app: &crate::proto::AppRef) -> Result<(), String> {
        Err("Moving apps isn't supported on this computer".into())
    }
    /// Close `app` here (it has opened on the other computer).
    fn close_app(&self, _app: &crate::proto::AppRef) {}
    /// While the other computer's mouse is dragging here: is a program's window being
    /// held by its title bar? `Ready` hands it over (and forgets it).
    fn app_hold(&self) -> AppHold {
        AppHold::Nothing
    }
    /// Show (or move) the window another computer is dragging over to this one.
    fn span_show(&self, _v: SpanView) {}
    /// A new picture (JPEG) for that window.
    fn span_frame(&self, _jpeg: Vec<u8>) {}
    /// Stop showing it, after `after_ms` (the real program may still be opening).
    fn span_hide(&self, _after_ms: u32) {}
    /// The program dragged over didn't open there: bring its window fully back on this
    /// screen.
    fn span_restore(&self) {}
    /// While the other computer's mouse drags a window here (we are being controlled):
    /// the pointer moved; start or stop showing the window over there as it goes past
    /// `edge` (the way back) or comes back.
    fn span_watch(&self, _edge: Edge) {}
    /// The other computer's mouse pushed a held window against `edge`: carry on moving it
    /// across with the moves that arrive (see `span_move`). True if that started.
    fn span_take(&self, _edge: Edge) -> bool {
        false
    }
    fn span_move(&self, _dx: i32, _dy: i32) {}
    /// The other computer's button went up during a drag across (or a preview).
    fn span_release(&self) {}
    /// Esc during a drag across: keep the window here.
    fn span_cancel(&self) {}
    /// Second-monitor mode works here (both ends need it).
    fn mirror_supported(&self) -> bool {
        false
    }
    /// The owner: the window that was just let go over `edge` stays here, parked past
    /// that edge, and its pictures start coming as `MirrorFrame`. Its id and size.
    fn mirror_adopt(&self, _edge: Edge) -> Option<(u64, u32, u32)> {
        None
    }
    /// The owner: stop mirroring `id` and put the window back on screen; at `home`
    /// (a fraction along the edge it was parked past), or in the middle.
    fn mirror_release(&self, _id: u64, _home: Option<f32>) {}
    /// The owner: window `id` is shown `k` times its size over there (a screen of another
    /// resolution), so its frames need be no bigger than that.
    fn mirror_scale(&self, id: u64, k: f32) {
        crate::mirror::set_scale(id, k);
    }
    /// The owner: the pointer comes onto window `id` at (`rx`, `ry`) inside it. The
    /// window slides onto this screen; returns where that point is now on the screen.
    /// From here the platform tells when the pointer leaves it (`PortalOut`, for this
    /// computer's own mouse; `portal_outside`, for moves that arrive from the other one).
    fn portal_begin(&self, _id: u64, _rx: i32, _ry: i32) -> Option<(i32, i32)> {
        None
    }
    /// The owner: the pointer has left the window: park it again.
    fn portal_end(&self) {}
    /// The owner, driven from the other computer: if the pointer is outside the window
    /// it was on (and no button is down), where, relative to the window.
    fn portal_outside(&self) -> Option<(i32, i32)> {
        None
    }
    /// The mirror end: show window `id` of the other computer (`w` x `h`) where the
    /// picture of it being dragged across was let go. `entry`: the edge it came in by.
    fn mirror_create(&self, _id: u64, _w: u32, _h: u32, _entry: Edge) -> bool {
        false
    }
    fn mirror_frame(&self, _id: u64, _w: u32, _h: u32, _jpeg: Vec<u8>) {}
    fn mirror_close(&self, _id: u64) {}
    /// Move mirror `id` by (`dx`, `dy`). When `done` and it was dragged more than half
    /// over the edge it came in by, it is closed and the fraction along that edge comes
    /// back (the window goes home).
    fn mirror_move(&self, _id: u64, _dx: i32, _dy: i32, _done: bool) -> Option<f32> {
        None
    }
    /// The mirror end, controlled from the other computer: is the pointer on one of the
    /// mirrors (on top, no button down)? Which one and where inside it.
    fn mirror_at_cursor(&self) -> Option<(u64, i32, i32)> {
        None
    }
    /// Draw the stand-in pointer on mirror `id` (or take it away).
    fn mirror_cursor(&self, _id: u64, _at: Option<(i32, i32)>) {}
    /// Where (`rx`, `ry`) inside mirror `id` is on this screen.
    fn mirror_point(&self, _id: u64, _rx: i32, _ry: i32) -> Option<(i32, i32)> {
        None
    }
    /// Puts files in the Recycle Bin / Trash (undo of files that arrived). True if done.
    fn trash(&self, _paths: &[std::path::PathBuf]) -> bool {
        false
    }
    /// Hide the real pointer without swallowing input (it is drawn on a mirror instead).
    fn pointer_hidden(&self, _on: bool) {}
    /// How this computer is doing right now. Called every few seconds: must be quick
    /// (anything slow, like accessories, is read in the background and cached).
    fn device_info(&self) -> crate::proto::DeviceInfo {
        crate::proto::DeviceInfo::default()
    }
    /// Lock this computer, or put it to sleep.
    fn power(&self, _a: crate::proto::PowerAction) {}
    /// The programs open here that could move to another computer (not browsers: their
    /// pages can't be read for every window). At most a handful.
    fn open_apps(&self) -> Vec<crate::proto::AppRef> {
        Vec::new()
    }
    /// A soft glow along `edge` at `frac`, where the cursor just crossed, in `rgb`.
    fn edge_glow(&self, _edge: Edge, _frac: f32, _rgb: [u8; 3]) {}
    /// How long the cursor must keep pushing against the edge before it crosses.
    fn set_switch_delay(&self, _ms: u32) {}
    /// Called once at startup; the platform pushes `CaptureEvent`s here.
    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>);
}

/// What the press under the cursor is holding, for moving programs.
#[derive(Debug, Clone, PartialEq)]
pub enum AppHold {
    Nothing,
    /// A browser's page address is still being read.
    Reading,
    Ready(crate::proto::AppRef),
}
