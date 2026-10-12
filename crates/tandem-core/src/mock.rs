//! In-memory `Platform` for tests (and for developing the UI without hooks).

use crate::platform::*;
use crate::proto::{Edge, InputEvent};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

pub struct MockState {
    pub screen: Screen,
    pub cursor: (i32, i32),
    pub capturing: bool,
    pub active_edge: Option<Edge>,
    pub active_edges: Vec<Edge>,
    pub injected: Vec<InputEvent>,
    pub clipboard: Option<ClipContent>,
    pub releases: u32,
    /// What `drop_dir_at` answers (the folder under the cursor, in a test).
    pub drop_dir: Option<std::path::PathBuf>,
    pub drag_files: bool,
}

pub struct MockPlatform {
    pub st: Mutex<MockState>,
    sink: Mutex<Option<UnboundedSender<CaptureEvent>>>,
}

impl MockPlatform {
    pub fn new(w: i32, h: i32) -> Arc<Self> {
        Arc::new(Self {
            st: Mutex::new(MockState {
                screen: Screen {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                },
                cursor: (w / 2, h / 2),
                capturing: false,
                active_edge: None,
                active_edges: vec![],
                injected: vec![],
                clipboard: None,
                releases: 0,
                drop_dir: None,
                drag_files: true,
            }),
            sink: Mutex::new(None),
        })
    }
    pub fn emit(&self, ev: CaptureEvent) {
        if let Some(s) = &*self.sink.lock().unwrap() {
            let _ = s.send(ev);
        }
    }
    /// Simulate the user pressing Ctrl+C.
    pub fn user_copy(&self, c: ClipContent) {
        self.st.lock().unwrap().clipboard = Some(c);
        self.emit(CaptureEvent::ClipboardChanged);
    }
    /// Simulate dragging files to a screen edge and over to the next computer.
    pub fn user_drag_to_edge(&self, edge: Edge, frac: f32, paths: Vec<std::path::PathBuf>) {
        self.st.lock().unwrap().capturing = true;
        self.emit(CaptureEvent::DragCrossed { edge, frac, paths });
    }
    /// Simulate the hook noticing the cursor pushing through an edge.
    pub fn user_push_edge(&self, edge: Edge, frac: f32) {
        self.st.lock().unwrap().capturing = true;
        self.emit(CaptureEvent::Crossed { edge, frac });
    }
}

impl Platform for MockPlatform {
    fn set_drag_files(&self, on: bool) {
        self.st.lock().unwrap().drag_files = on;
    }
    fn drop_dir_at(&self, _x: i32, _y: i32) -> Option<std::path::PathBuf> {
        self.st.lock().unwrap().drop_dir.clone()
    }
    fn screen(&self) -> Screen {
        self.st.lock().unwrap().screen
    }
    fn set_active_edges(&self, edges: &[Edge]) {
        let mut s = self.st.lock().unwrap();
        s.active_edges = edges.to_vec();
        s.active_edge = edges.first().copied();
    }
    fn begin_capture(&self) {
        let mut s = self.st.lock().unwrap();
        s.capturing = true;
        s.cursor = s.screen.center();
    }
    fn end_capture(&self, x: i32, y: i32) {
        let mut s = self.st.lock().unwrap();
        s.capturing = false;
        s.cursor = (x, y);
    }
    fn inject(&self, ev: &InputEvent) {
        let mut s = self.st.lock().unwrap();
        if let InputEvent::MouseMove { dx, dy } = ev {
            let (w, h) = (s.screen.width, s.screen.height);
            s.cursor = (
                (s.cursor.0 + dx).clamp(0, w - 1),
                (s.cursor.1 + dy).clamp(0, h - 1),
            );
        }
        s.injected.push(ev.clone());
    }
    fn cursor_pos(&self) -> (i32, i32) {
        self.st.lock().unwrap().cursor
    }
    fn set_cursor(&self, x: i32, y: i32) {
        self.st.lock().unwrap().cursor = (x, y);
    }
    fn release_all(&self) {
        self.st.lock().unwrap().releases += 1;
    }
    fn clipboard_get(&self) -> Option<ClipContent> {
        self.st.lock().unwrap().clipboard.clone()
    }
    fn clipboard_set(&self, c: &ClipContent) {
        self.st.lock().unwrap().clipboard = Some(c.clone());
        // Like the real OS, setting the clipboard notifies listeners.
        self.emit(CaptureEvent::ClipboardChanged);
    }
    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>) {
        *self.sink.lock().unwrap() = Some(tx);
    }
}
