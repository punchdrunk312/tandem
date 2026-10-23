//! In-memory `Platform` for tests (and for developing the UI without hooks).

use crate::platform::*;
use crate::proto::{AppRef, Edge, InputEvent};
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
    /// What `take_drag_files` answers once (the selection under the press, in a test).
    pub drag_ready: Option<Vec<std::path::PathBuf>>,
    pub carrying: bool,
    pub drag_cancels: u32,
    pub move_apps: bool,
    /// What `confirm_move_app` answers (the person at this computer, in a test).
    pub confirm_move: bool,
    pub confirm_asked: Vec<(String, AppRef)>,
    pub launched: Vec<AppRef>,
    pub launch_error: Option<String>,
    pub closed: Vec<AppRef>,
    /// A window held by its title bar while this computer is being controlled.
    pub app_hold: Option<AppRef>,
    /// A window dragged over from the other computer, as shown here.
    pub span_shown: Vec<SpanView>,
    pub span_frames: usize,
    /// Each `span_hide`, with its delay.
    pub span_hidden: Vec<u32>,
    pub span_restores: u32,
    /// A window held by the other computer's mouse here, that can hang across.
    pub span_window: Option<AppRef>,
    pub span_taken: Option<Edge>,
    pub span_moved_x: i32,
    /// What `device_info` answers.
    pub device: crate::proto::DeviceInfo,
    pub power_done: Vec<crate::proto::PowerAction>,
    /// What `open_apps` answers.
    pub open: Vec<AppRef>,
    pub glows: Vec<(Edge, [u8; 3])>,
    /// Second-monitor mode works here.
    pub mirror_ok: bool,
    /// Owner: windows kept here and shown elsewhere; and given back (with `home`).
    pub adopted: Vec<u64>,
    pub mirror_released: Vec<(u64, Option<f32>)>,
    /// Owner: the window slid in for the pointer (id, left, top, width, height).
    pub portal_window: Option<(u64, i32, i32, i32, i32)>,
    pub portal_ends: u32,
    /// Mirror end: mirrors shown here (id, left, top, width, height).
    pub mirrors: Vec<(u64, i32, i32, i32, i32)>,
    pub mirrors_closed: Vec<u64>,
    pub mirror_frames: usize,
    pub mirror_cursor: Option<(u64, i32, i32)>,
    pub pointer_hidden: bool,
    /// Files put in the bin (undo).
    pub trashed: Vec<std::path::PathBuf>,
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
                drag_ready: None,
                carrying: false,
                drag_cancels: 0,
                move_apps: false,
                confirm_move: true,
                confirm_asked: vec![],
                launched: vec![],
                launch_error: None,
                closed: vec![],
                app_hold: None,
                span_shown: vec![],
                span_frames: 0,
                span_hidden: vec![],
                span_restores: 0,
                span_window: None,
                span_taken: None,
                span_moved_x: 0,
                device: Default::default(),
                power_done: vec![],
                open: vec![],
                glows: vec![],
                mirror_ok: false,
                adopted: vec![],
                mirror_released: vec![],
                portal_window: None,
                portal_ends: 0,
                mirrors: vec![],
                mirrors_closed: vec![],
                mirror_frames: 0,
                mirror_cursor: None,
                pointer_hidden: false,
                trashed: vec![],
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
    /// Simulate dragging a program's window to a connected edge and holding it there.
    pub fn user_drag_app_to_edge(&self, edge: Edge, app: AppRef) {
        self.emit(CaptureEvent::AppDragged { edge, app });
    }
    /// Simulate this computer's own mouse going onto mirror `id` (the hook swallows it).
    pub fn user_onto_mirror(&self, id: u64, rx: i32, ry: i32) {
        self.st.lock().unwrap().capturing = true;
        self.emit(CaptureEvent::PortalIn { id, rx, ry });
    }
    /// Simulate this computer's own mouse leaving the real window it is on.
    pub fn user_off_window(&self, rx: i32, ry: i32) {
        self.st.lock().unwrap().capturing = true;
        self.emit(CaptureEvent::PortalOut { rx, ry });
    }
    /// Simulate the hook noticing the cursor pushing through an edge.
    pub fn user_push_edge(&self, edge: Edge, frac: f32) {
        self.st.lock().unwrap().capturing = true;
        self.emit(CaptureEvent::Crossed { edge, frac });
    }
}

impl Platform for MockPlatform {
    fn set_move_apps(&self, on: bool) {
        self.st.lock().unwrap().move_apps = on;
    }
    fn confirm_move_app(&self, to: &str, app: &AppRef) -> bool {
        let mut s = self.st.lock().unwrap();
        s.confirm_asked.push((to.to_string(), app.clone()));
        s.confirm_move
    }
    fn launch_app(&self, app: &AppRef) -> Result<(), String> {
        let mut s = self.st.lock().unwrap();
        if let Some(e) = s.launch_error.clone() {
            return Err(e);
        }
        s.launched.push(app.clone());
        Ok(())
    }
    fn close_app(&self, app: &AppRef) {
        self.st.lock().unwrap().closed.push(app.clone());
    }
    fn app_hold(&self) -> crate::platform::AppHold {
        match self.st.lock().unwrap().app_hold.take() {
            Some(a) => crate::platform::AppHold::Ready(a),
            None => crate::platform::AppHold::Nothing,
        }
    }
    fn take_drag_files(&self) -> Option<Vec<std::path::PathBuf>> {
        self.st.lock().unwrap().drag_ready.take()
    }
    fn cancel_drag(&self) {
        self.st.lock().unwrap().drag_cancels += 1;
    }
    fn set_carrying(&self, on: bool) {
        self.st.lock().unwrap().carrying = on;
    }
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
    fn span_show(&self, v: SpanView) {
        self.st.lock().unwrap().span_shown.push(v);
    }
    fn span_frame(&self, _jpeg: Vec<u8>) {
        self.st.lock().unwrap().span_frames += 1;
    }
    fn span_hide(&self, after_ms: u32) {
        self.st.lock().unwrap().span_hidden.push(after_ms);
    }
    fn span_restore(&self) {
        self.st.lock().unwrap().span_restores += 1;
    }
    fn device_info(&self) -> crate::proto::DeviceInfo {
        self.st.lock().unwrap().device.clone()
    }
    fn power(&self, a: crate::proto::PowerAction) {
        self.st.lock().unwrap().power_done.push(a);
    }
    fn open_apps(&self) -> Vec<AppRef> {
        self.st.lock().unwrap().open.clone()
    }
    fn edge_glow(&self, edge: Edge, _frac: f32, rgb: [u8; 3]) {
        self.st.lock().unwrap().glows.push((edge, rgb));
    }
    fn span_take(&self, edge: Edge) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.span_window.is_none() {
            return false;
        }
        st.span_taken = Some(edge);
        st.span_moved_x = 0;
        drop(st);
        self.emit(CaptureEvent::SpanBegin {
            edge,
            w: 1200,
            h: 800,
        });
        true
    }
    fn span_move(&self, dx: i32, _dy: i32) {
        let mut st = self.st.lock().unwrap();
        if st.span_taken.is_some() {
            st.span_moved_x += dx.abs();
            let c = st.span_moved_x;
            drop(st);
            self.emit(CaptureEvent::SpanMove {
                crossed: c,
                along: 0.5,
                gx: 300,
                gy: 15,
            });
        }
    }
    fn span_release(&self) {
        let mut st = self.st.lock().unwrap();
        if st.span_taken.take().is_none() {
            return;
        }
        // Let go more than halfway over the 1200-px window: it moves.
        let moved = st.span_moved_x * 2 > 1200;
        let app = st.span_window.take();
        drop(st);
        self.emit(CaptureEvent::SpanEnd {
            moved,
            app,
            frac: 0.5,
        });
    }
    fn span_cancel(&self) {
        if self.st.lock().unwrap().span_taken.take().is_some() {
            self.emit(CaptureEvent::SpanEnd {
                moved: false,
                app: None,
                frac: 0.0,
            });
        }
    }
    fn mirror_supported(&self) -> bool {
        self.st.lock().unwrap().mirror_ok
    }
    fn mirror_adopt(&self, _edge: Edge) -> Option<(u64, u32, u32)> {
        let mut st = self.st.lock().unwrap();
        st.adopted.push(7);
        Some((7, 400, 300))
    }
    fn mirror_release(&self, id: u64, home: Option<f32>) {
        let mut st = self.st.lock().unwrap();
        st.mirror_released.push((id, home));
        if st.portal_window.is_some_and(|w| w.0 == id) {
            st.portal_window = None;
        }
    }
    /// The window slides in at (300, 250), 400 x 300.
    fn portal_begin(&self, id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
        let mut st = self.st.lock().unwrap();
        if !st.adopted.contains(&id) {
            return None;
        }
        st.portal_window = Some((id, 300, 250, 400, 300));
        Some((300 + rx, 250 + ry))
    }
    fn portal_end(&self) {
        let mut st = self.st.lock().unwrap();
        st.portal_window = None;
        st.portal_ends += 1;
    }
    fn portal_outside(&self) -> Option<(i32, i32)> {
        let st = self.st.lock().unwrap();
        let (_, x, y, w, h) = st.portal_window?;
        let (cx, cy) = st.cursor;
        let inside = cx >= x && cy >= y && cx < x + w && cy < y + h;
        (!inside).then_some((cx - x, cy - y))
    }
    /// Mirrors appear at (0, 100).
    fn mirror_create(&self, id: u64, w: u32, h: u32, _entry: Edge) -> bool {
        let mut st = self.st.lock().unwrap();
        if !st.mirror_ok {
            return false;
        }
        st.mirrors.push((id, 0, 100, w as i32, h as i32));
        true
    }
    fn mirror_frame(&self, _id: u64, _w: u32, _h: u32, _jpeg: Vec<u8>) {
        self.st.lock().unwrap().mirror_frames += 1;
    }
    fn mirror_close(&self, id: u64) {
        let mut st = self.st.lock().unwrap();
        st.mirrors.retain(|m| m.0 != id);
        st.mirrors_closed.push(id);
    }
    fn mirror_move(&self, id: u64, dx: i32, dy: i32, done: bool) -> Option<f32> {
        let mut st = self.st.lock().unwrap();
        let m = st.mirrors.iter_mut().find(|m| m.0 == id)?;
        m.1 += dx;
        m.2 += dy;
        if done && m.1 * 2 < -m.3 {
            st.mirrors.retain(|m| m.0 != id);
            return Some(0.5);
        }
        None
    }
    fn mirror_at_cursor(&self) -> Option<(u64, i32, i32)> {
        let st = self.st.lock().unwrap();
        let (cx, cy) = st.cursor;
        st.mirrors
            .iter()
            .find(|m| cx >= m.1 && cy >= m.2 && cx < m.1 + m.3 && cy < m.2 + m.4)
            .map(|m| (m.0, cx - m.1, cy - m.2))
    }
    fn mirror_cursor(&self, id: u64, at: Option<(i32, i32)>) {
        self.st.lock().unwrap().mirror_cursor = at.map(|(x, y)| (id, x, y));
    }
    fn mirror_point(&self, id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
        let st = self.st.lock().unwrap();
        st.mirrors
            .iter()
            .find(|m| m.0 == id)
            .map(|m| (m.1 + rx, m.2 + ry))
    }
    fn trash(&self, paths: &[std::path::PathBuf]) -> bool {
        self.st.lock().unwrap().trashed.extend(paths.iter().cloned());
        true
    }
    fn pointer_hidden(&self, on: bool) {
        self.st.lock().unwrap().pointer_hidden = on;
    }
    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>) {
        *self.sink.lock().unwrap() = Some(tx);
    }
}
