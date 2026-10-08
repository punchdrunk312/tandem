// Island window: placement on the display, the two window sizes (full panel /
// invisible wake strip), click-through and the cursor poll.
//
// Adapted from Coucou's Windows island. There is no notch on a PC, so the island
// is a black shape drawn at the top centre of the main display inside a
// borderless, transparent, always-on-top window that never takes focus.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

/// Logical size of the full window — the largest island view.
pub const PANEL_W: f64 = 720.0;
pub const PANEL_H: f64 = 320.0;
/// Logical size of the invisible strip that wakes the island when it is hidden.
pub const STRIP_W: f64 = 240.0;
pub const STRIP_H: f64 = 6.0;

pub const WINDOW_LABEL: &str = "island";

/// Margin around the island shape that still takes the mouse, in logical px.
/// Kept tiny: everything outside the shape must click through to the app below.
const HIT_MARGIN: f64 = 3.0;

#[derive(Serialize, Clone)]
pub struct CursorPayload {
    pub x: f64,
    pub y: f64,
}

/// The island shape in window-logical coordinates, pushed by the front end.
/// The poll thread owns the click-through decision so it lands in the same 16 ms
/// tick as the cursor read — an IPC round trip here loses clicks.
#[derive(Clone, Copy, Default)]
pub struct IslandRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Wakes / parks the cursor poll thread so a hidden island costs literally nothing.
pub struct PollGate {
    active: Mutex<bool>,
    cv: Condvar,
    pub collapsed: AtomicBool,
    pub rect: Mutex<IslandRect>,
    /// Mirrors the window flag so we only call into Win32 when it changes.
    ignoring: AtomicBool,
    /// Which screen edge leads to the connected computer: 0 none, 1 left, 2 right,
    /// 3 top, 4 bottom. Drives the drag-across-the-edge strip.
    pub peer_edge: AtomicU8,
    /// Island goes to sleep while a full-screen app is in front.
    pub hide_fullscreen: AtomicBool,
    /// Follow the cursor to whichever display it is on (otherwise: primary display).
    pub monitor_cursor: AtomicBool,
    /// While `now_ms() < reveal_until_ms` the island may show even in a full-screen app.
    pub reveal_until_ms: AtomicU64,
}

impl Default for PollGate {
    fn default() -> Self {
        Self::new()
    }
}

impl PollGate {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(false),
            cv: Condvar::new(),
            collapsed: AtomicBool::new(true),
            rect: Mutex::new(IslandRect::default()),
            ignoring: AtomicBool::new(false),
            peer_edge: AtomicU8::new(0),
            hide_fullscreen: AtomicBool::new(true),
            monitor_cursor: AtomicBool::new(false),
            reveal_until_ms: AtomicU64::new(0),
        }
    }

    pub fn pref(&self) -> &'static str {
        if self.monitor_cursor.load(Ordering::Relaxed) {
            "cursor"
        } else {
            "primary"
        }
    }

    pub fn set_rect(&self, rect: IslandRect) {
        *self.rect.lock().unwrap() = rect;
    }

    /// Forces the next poll tick to re-apply the flag (after a window resize).
    pub fn forget_ignore_state(&self) {
        self.ignoring.store(false, Ordering::Relaxed);
    }

    pub fn set_active(&self, on: bool) {
        let mut guard = self.active.lock().unwrap();
        *guard = on;
        self.cv.notify_all();
    }

    fn wait_until_active(&self) {
        let mut guard = self.active.lock().unwrap();
        while !*guard {
            guard = self.cv.wait(guard).unwrap();
        }
    }

    fn is_active(&self) -> bool {
        *self.active.lock().unwrap()
    }
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

// ------------------------------------------------------------------ Win32 bits

#[cfg(windows)]
mod sys {
    use tauri::WebviewWindow;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };

    /// True while a full-screen app (game, F11 video, presentation) is in front.
    pub fn fullscreen_app_active() -> bool {
        tandem_core::win::fullscreen_foreground()
    }

    /// Window class of whatever is in front ("CabinetWClass" is File Explorer).
    pub fn foreground_class() -> String {
        tandem_core::win::foreground_class()
    }

    /// Shows the window without activating it, so a game never loses focus.
    pub fn show_no_activate(win: &WebviewWindow) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNA};
        if let Some(hwnd) = hwnd_of(win) {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNA);
            }
        }
    }

    pub fn cursor_physical() -> Option<(f64, f64)> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p).ok()? };
        Some((p.x as f64, p.y as f64))
    }

    /// True while the left mouse button is held — the only signal we get that a
    /// drag might be in flight before it reaches the window.
    pub fn left_button_down() -> bool {
        unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
    }

    fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
        let raw = win.hwnd().ok()?.0 as isize;
        if raw == 0 {
            return None;
        }
        Some(HWND(raw as *mut _))
    }

    /// WS_EX_NOACTIVATE keeps clicks from stealing focus; WS_EX_TOOLWINDOW keeps the
    /// island out of Alt-Tab.
    pub fn make_non_activating(win: &WebviewWindow) {
        let Some(hwnd) = hwnd_of(win) else { return };
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let want = ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        }
    }
}

/// Non-Windows stand-ins so the crate still type-checks for UI development.
#[cfg(not(windows))]
mod sys {
    use tauri::WebviewWindow;
    pub fn cursor_physical() -> Option<(f64, f64)> {
        None
    }
    pub fn left_button_down() -> bool {
        false
    }
    pub fn fullscreen_app_active() -> bool {
        false
    }
    pub fn foreground_class() -> String {
        String::new()
    }
    pub fn show_no_activate(win: &WebviewWindow) {
        let _ = win.show();
    }
    pub fn make_non_activating(_: &WebviewWindow) {}
}

pub use sys::make_non_activating;

// ------------------------------------------------------------------ geometry

fn monitor_contains(m: &Monitor, x: f64, y: f64) -> bool {
    let p = m.position();
    let s = m.size();
    x >= p.x as f64
        && x < (p.x + s.width as i32) as f64
        && y >= p.y as f64
        && y < (p.y + s.height as i32) as f64
}

/// The display the island lives on: the primary one, or the one under the cursor.
fn target_monitor(app: &AppHandle, pref: &str) -> Option<Monitor> {
    let monitors = app.available_monitors().ok()?;
    if pref == "cursor" {
        if let Some((cx, cy)) = sys::cursor_physical() {
            if let Some(m) = monitors.iter().find(|m| monitor_contains(m, cx, cy)) {
                return Some(m.clone());
            }
        }
    }
    app.primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.into_iter().next())
}

/// Places and sizes the window. `collapsed` picks the wake strip instead of the panel.
pub fn apply_geometry(app: &AppHandle, pref: &str, collapsed: bool) {
    let Some(win) = window(app) else { return };
    let Some(m) = target_monitor(app, pref) else {
        return;
    };

    let scale = m.scale_factor();
    let mp = *m.position();
    let ms = *m.size();

    let (lw, lh) = if collapsed {
        (STRIP_W, STRIP_H)
    } else {
        (PANEL_W, PANEL_H)
    };
    let pw = (lw * scale).round().max(1.0) as u32;
    let ph = (lh * scale).round().max(1.0) as u32;
    let x = mp.x + (ms.width as i32 - pw as i32) / 2;
    let y = mp.y;

    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_position(PhysicalPosition::new(x, y));
    // Moving across displays can rescale the window: re-assert the physical size.
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_always_on_top(true);
}

/// Position, size and scale of the monitor the island lives on. Any change here
/// means the island has to be placed again.
fn current_screen_key(app: &AppHandle, pref: &str) -> Option<(i32, i32, u32, u32, u64)> {
    let m = target_monitor(app, pref)?;
    let p = m.position();
    let size = m.size();
    Some((
        p.x,
        p.y,
        size.width,
        size.height,
        m.scale_factor().to_bits(),
    ))
}

// ------------------------------------------------------------------ edge drop strip

/// Logical thickness of the strip that appears along the edge facing the other
/// computer while a drag is in flight. Dropping a file on it sends the file.
const EDGE_STRIP: f64 = 72.0;
pub const EDGE_LABEL: &str = "edge";

/// Bounding box of all displays, physical pixels: (x, y, width, height).
fn virtual_screen(app: &AppHandle) -> Option<(i32, i32, i32, i32)> {
    let ms = app.available_monitors().ok()?;
    let mut it = ms.iter();
    let first = it.next()?;
    let (mut l, mut t) = (first.position().x, first.position().y);
    let (mut r, mut b) = (
        l + first.size().width as i32,
        t + first.size().height as i32,
    );
    for m in it {
        l = l.min(m.position().x);
        t = t.min(m.position().y);
        r = r.max(m.position().x + m.size().width as i32);
        b = b.max(m.position().y + m.size().height as i32);
    }
    Some((l, t, r - l, b - t))
}

/// Pixels between the cursor and `edge` of the virtual screen (negative: outside).
fn edge_distance(edge: u8, vs: (i32, i32, i32, i32), cx: f64, cy: f64) -> f64 {
    match edge {
        1 => cx - vs.0 as f64,
        2 => (vs.0 + vs.2 - 1) as f64 - cx,
        3 => cy - vs.1 as f64,
        _ => (vs.1 + vs.3 - 1) as f64 - cy,
    }
}

fn show_edge_strip(app: &AppHandle, edge: u8, vs: (i32, i32, i32, i32), scale: f64) {
    let Some(w) = app.get_webview_window(EDGE_LABEL) else {
        return;
    };
    let t = (EDGE_STRIP * scale).round() as i32;
    let (x, y, wd, ht) = match edge {
        1 => (vs.0, vs.1, t, vs.3),
        2 => (vs.0 + vs.2 - t, vs.1, t, vs.3),
        3 => (vs.0, vs.1, vs.2, t),
        _ => (vs.0, vs.1 + vs.3 - t, vs.2, t),
    };
    let _ = w.set_size(PhysicalSize::new(wd.max(1) as u32, ht.max(1) as u32));
    let _ = w.set_position(PhysicalPosition::new(x, y));
    let _ = w.set_ignore_cursor_events(false);
    let _ = w.emit(
        "edge-side",
        match edge {
            1 => "left",
            2 => "right",
            3 => "top",
            _ => "bottom",
        },
    );
    sys::show_no_activate(&w);
    let _ = w.set_always_on_top(true);
}

fn hide_edge_strip(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(EDGE_LABEL) {
        let _ = w.set_ignore_cursor_events(true);
        let _ = w.hide();
    }
}

pub fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Bring the island back, whatever state it is in: the tray icon, the hotkey, starting
/// Tandem a second time and the full-screen "rest at the top" gesture all end up here.
/// Re-asserts geometry and z-order so even a mis-placed window is recovered.
pub fn summon(app: &AppHandle, gate: &PollGate) {
    gate.reveal_until_ms
        .store(now_ms() + 10_000, Ordering::Relaxed);
    if let Some(win) = window(app) {
        apply_geometry(app, gate.pref(), gate.collapsed.load(Ordering::Relaxed));
        gate.forget_ignore_state();
        sys::make_non_activating(&win);
        sys::show_no_activate(&win);
        let _ = win.set_always_on_top(true);
    }
    gate.set_active(true);
    let _ = app.emit_to(WINDOW_LABEL, "summon", ());
}

/// Window classes whose drags are file drags: Explorer windows and the desktop.
fn is_file_surface(class: &str) -> bool {
    matches!(
        class,
        "CabinetWClass" | "ExploreWClass" | "Progman" | "WorkerW"
    )
}

/// Watches the cursor and tells the island page what is going on — at ~60 Hz while the
/// island is on, parked on a condvar otherwise.
///
/// Events sent to the island window:
/// * `cursor`    window-logical cursor position (only while it matters)
/// * `wake`      cursor rested against the top edge near the middle: show the pill
/// * `drag-hint` a file drag started in Explorer / on the desktop while connected
/// * `quiet`     a full-screen app is in front: stay out of the way
/// * `summon`    explicit request to show (tray, hotkey, full-screen gesture)
///
/// The window itself is never hidden: it is transparent and click-through everywhere
/// except over the island shape, so it can't "disappear" — and it can't eat clicks.
pub fn spawn_cursor_poll(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        loop {
            gate.wait_until_active();

            let mut ticks: u32 = 0;
            let mut geo: Option<(f64, f64, f64, f64, f64)> = None;
            let mut vs: Option<(i32, i32, i32, i32)> = None;
            let mut last = (f64::MIN, f64::MIN);
            let mut last_key = (0.0, 0.0, 0.0, 0.0, false, true);
            let mut last_collapsed = true;
            let mut was_near = false;

            // Press / drag tracking.
            let mut was_down = false;
            let mut press_inside = false;
            let mut press_pt = (0.0, 0.0);
            let mut drag_checked = false;
            let mut drag_started = false;
            let mut hint_on = false;
            let mut edge_shown = false;

            // Full-screen handling and edge wake.
            let mut fs = false;
            let mut quiet_sent: Option<bool> = None;
            let mut dwell: Option<Instant> = None;
            let mut wake_armed = true;

            while gate.is_active() {
                std::thread::sleep(Duration::from_millis(16));
                ticks = ticks.wrapping_add(1);

                // Monitors get plugged in, unplugged, rearranged and rescaled: re-place
                // the island when that happens. Checked about twice a second.
                if ticks.is_multiple_of(30) {
                    let now = current_screen_key(&app, gate.pref());
                    if now.is_some() && now != last_screen {
                        let first = last_screen.is_none();
                        last_screen = now;
                        if !first {
                            let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                        }
                    }
                }
                if ticks.is_multiple_of(15) {
                    fs = gate.hide_fullscreen.load(Ordering::Relaxed)
                        && sys::fullscreen_app_active();
                }
                if ticks.is_multiple_of(30) || vs.is_none() {
                    vs = virtual_screen(&app);
                }

                let Some(win) = window(&app) else { continue };
                let Some((cx, cy)) = sys::cursor_physical() else {
                    continue;
                };

                // Window origin / scale / size change rarely and each query is a round
                // trip to the UI thread: refresh a few times a second, or right after a
                // resize.
                let collapsed = gate.collapsed.load(Ordering::Relaxed);
                if geo.is_none() || ticks.is_multiple_of(10) || collapsed != last_collapsed {
                    last_collapsed = collapsed;
                    if let (Ok(o), Ok(sz)) = (win.outer_position(), win.inner_size()) {
                        let sc = win.scale_factor().unwrap_or(1.0);
                        geo = Some((
                            o.x as f64,
                            o.y as f64,
                            sc,
                            sz.width as f64 / sc,
                            sz.height as f64 / sc,
                        ));
                    }
                }
                let Some((ox, oy, scale, w, h)) = geo else {
                    continue;
                };
                let x = (cx - ox) / scale;
                let y = (cy - oy) / scale;
                let mid = ox + w * scale / 2.0;
                let inside = x >= 0.0 && x <= w && y >= 0.0 && y <= h;

                // ---- full-screen: stay quiet unless asked ----------------------------
                let now = now_ms();
                let revealed = now < gate.reveal_until_ms.load(Ordering::Relaxed);
                let quiet = fs && !revealed;
                if quiet_sent != Some(quiet) {
                    quiet_sent = Some(quiet);
                    let _ = app.emit_to(WINDOW_LABEL, "quiet", quiet);
                }
                if fs {
                    // Resting at the very top, near the middle, brings the island back.
                    let at_top = cy <= oy + 2.0 * scale && (cx - mid).abs() <= 210.0 * scale;
                    if at_top && quiet {
                        let t = *dwell.get_or_insert_with(Instant::now);
                        if t.elapsed() >= Duration::from_millis(450) {
                            dwell = None;
                            summon(&app, &gate);
                        }
                    } else {
                        dwell = None;
                    }
                    // Keep it awake while the cursor is around the island.
                    if revealed
                        && (-4.0..=PANEL_H + 40.0).contains(&y)
                        && (cx - mid).abs() <= (PANEL_W / 2.0) * scale
                    {
                        gate.reveal_until_ms.store(now + 4_000, Ordering::Relaxed);
                    }
                }

                // ---- buttons, drags ---------------------------------------------------
                let down = sys::left_button_down();
                if down && !was_down {
                    press_pt = (cx, cy);
                    press_inside = inside;
                    drag_checked = false;
                    drag_started = false;
                }
                if !down {
                    press_inside = false;
                    drag_checked = false;
                    drag_started = false;
                }
                was_down = down;
                // A file drag: the press started in Explorer / on the desktop and the
                // cursor has since travelled a little. Checked once per press.
                if down && !press_inside && !drag_checked {
                    let moved = ((cx - press_pt.0).powi(2) + (cy - press_pt.1).powi(2)).sqrt();
                    if moved > 24.0 * scale {
                        drag_checked = true;
                        drag_started = is_file_surface(&sys::foreground_class());
                    }
                }
                let connected_edge = gate.peer_edge.load(Ordering::Relaxed);

                // The island opens its drop view as soon as a file drag starts.
                let want_hint = drag_started && connected_edge != 0 && !quiet;
                if want_hint != hint_on {
                    hint_on = want_hint;
                    let _ = app.emit_to(WINDOW_LABEL, "drag-hint", want_hint);
                }

                // Dragging towards the edge that leads to the other computer: a drop
                // strip appears there.
                if let Some(v) = vs {
                    let d = if connected_edge != 0 {
                        edge_distance(connected_edge, v, cx, cy)
                    } else {
                        -1.0
                    };
                    let reach = if edge_shown { 240.0 } else { 90.0 } * scale;
                    let want = drag_started && !fs && d >= 0.0 && d <= reach;
                    if want && !edge_shown {
                        edge_shown = true;
                        show_edge_strip(&app, connected_edge, v, scale);
                    } else if !want && edge_shown {
                        edge_shown = false;
                        hide_edge_strip(&app);
                    }
                }

                // ---- top edge wake ----------------------------------------------------
                // Touching the top edge near the middle brings up the pill, even though
                // the island itself is only a few pixels wide while it sleeps.
                let in_zone = cy <= oy + 1.0 * scale && (cx - mid).abs() <= 210.0 * scale;
                if in_zone && wake_armed && !quiet && collapsed {
                    wake_armed = false;
                    let _ = app.emit_to(WINDOW_LABEL, "wake", ());
                } else if cy > oy + 40.0 * scale {
                    wake_armed = true;
                }

                // ---- click-through and cursor feed -------------------------------------
                let r = *gate.rect.lock().unwrap();
                let key = (r.x, r.y, r.w, r.h, down, collapsed);
                if (x - last.0).abs() < 1.0 && (y - last.1).abs() < 1.0 && key == last_key {
                    continue;
                }
                last = (x, y);
                last_key = key;

                let on_island = !quiet
                    && r.w > 0.0
                    && x >= r.x - HIT_MARGIN
                    && x <= r.x + r.w + HIT_MARGIN
                    && y >= r.y - HIT_MARGIN
                    && y <= r.y + r.h + HIT_MARGIN;
                // A file being dragged has to find a drop target. Click-through hides the
                // window from OLE, so while a button that was pressed *elsewhere* is held
                // over the window, the window takes the mouse. A press that started on the
                // window is an ordinary click on whatever is under it.
                let dragging = down && inside && !press_inside;
                let accept = on_island || dragging;
                if gate.ignoring.load(Ordering::Relaxed) == accept {
                    gate.ignoring.store(!accept, Ordering::Relaxed);
                    let _ = win.set_ignore_cursor_events(!accept);
                }

                // While the island sleeps it only needs the cursor near its wake strip.
                let near = !collapsed
                    || ((-8.0..=STRIP_H + 60.0).contains(&y)
                        && (-80.0..=STRIP_W + 80.0).contains(&x));
                if !quiet && (near || was_near) {
                    let _ = win.emit("cursor", CursorPayload { x, y });
                }
                was_near = near;
            }
        }
    });
}
