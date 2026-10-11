// Island window: placement on the display, the two window sizes (full panel /
// invisible wake strip), click-through and the cursor poll.
//
// Adapted from Coucou's Windows island. There is no notch on a PC, so the island
// is a black shape drawn at the top centre of the main display inside a
// borderless, transparent, always-on-top window that never takes focus.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_SPACE};
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

    pub fn space_down() -> bool {
        unsafe { (GetAsyncKeyState(VK_SPACE.0 as i32) as u16 & 0x8000) != 0 }
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
    pub fn space_down() -> bool {
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

    // Same place as last time: nothing to do (each call costs a visible layout pass).
    static LAST: Mutex<Option<(i32, i32, u32, u32)>> = Mutex::new(None);
    let placed = (x, y, pw, ph);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == Some(placed) {
            return;
        }
        *last = Some(placed);
    }

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

pub fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// How long a hotkey keeps the bar available inside a full-screen app.
const REVEAL_MS: u64 = 10_000;

/// A hotkey was pressed: `what` is "bar" (Ctrl+Alt+Z, the control bar) or "pill"
/// (Ctrl+Alt+U, the transfer bar). The page decides what to show or hide; this makes
/// sure the window is in place, on top and allowed to show even in a full-screen app.
pub fn hotkey(app: &AppHandle, gate: &PollGate, what: &'static str) {
    gate.reveal_until_ms
        .store(now_ms() + REVEAL_MS, Ordering::Relaxed);
    if let Some(win) = window(app) {
        // Always the display the user is on, whatever the "follow cursor" setting says.
        apply_geometry(app, "cursor", gate.collapsed.load(Ordering::Relaxed));
        gate.forget_ignore_state();
        sys::make_non_activating(&win);
        sys::show_no_activate(&win);
        let _ = win.set_always_on_top(true);
    }
    gate.set_active(true);
    let _ = app.emit_to(WINDOW_LABEL, "hotkey", what);
}

/// The page closed the bar again: a full-screen app gets its screen back at once.
pub fn end_reveal(gate: &PollGate) {
    gate.reveal_until_ms.store(0, Ordering::Relaxed);
}

/// Window classes whose drags are file drags: Explorer windows and the desktop.
fn is_file_surface(class: &str) -> bool {
    matches!(
        class,
        "CabinetWClass" | "ExploreWClass" | "Progman" | "WorkerW"
    )
}

/// `true` when `slot` is empty or older than `every`; stamps it.
fn due(slot: &mut Option<Instant>, every: Duration, now: Instant) -> bool {
    match slot {
        Some(t) if now.duration_since(*t) < every => false,
        _ => {
            *slot = Some(now);
            true
        }
    }
}

type Geo = (f64, f64, f64, f64, f64); // origin x, y (physical), scale, width, height (logical)

/// Watches the cursor on behalf of the page.
///
/// The bar is a one-pixel-line at rest and never pops up by itself, so this thread has
/// very little to do: while the window is collapsed and no mouse button is down it only
/// checks (twenty times a second) for a button press, plus a full-screen check a few times
/// a second. It speeds up to ~60 Hz only while the panel is open or a button is held.
///
/// Events sent to the island window:
/// * `cursor`     window-logical cursor position (only while the panel is open and near)
/// * `quiet`      a full-screen app is in front: stay out of the way
/// * `drag-hint`  Space was pressed while dragging a file in Explorer / on the desktop
///   (true), or the drag ended (false)
/// * `screen-changed`  displays changed: place the window again
/// * `hotkey`     (sent by [`hotkey`])
///
/// The window itself is never hidden: it is transparent and click-through everywhere
/// except over the bar's shape — and over its whole area while a drag hint is up, so the
/// drag has something to drop on.
pub fn spawn_cursor_poll(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        loop {
            gate.wait_until_active();

            let mut geo: Option<Geo> = None;
            let (mut geo_at, mut screen_at, mut fs_at) = (None, None, None);
            let mut fs = false;
            let mut quiet_sent: Option<bool> = None;
            let mut last = (f64::MIN, f64::MIN);
            let mut last_key = (0.0, 0.0, 0.0, 0.0, false, true);
            let mut last_collapsed = true;
            let mut was_near = false;
            let mut last_feed = Instant::now();
            let mut was_down = false;
            let mut was_space = false;
            let mut hint_on = false;

            while gate.is_active() {
                let fast = was_down || hint_on || !gate.collapsed.load(Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(if fast { 16 } else { 50 }));
                let now_i = Instant::now();
                let collapsed = gate.collapsed.load(Ordering::Relaxed);
                let down = sys::left_button_down();

                // Monitors get plugged in, unplugged, rearranged and rescaled: re-place
                // the island when that happens.
                if due(&mut screen_at, Duration::from_millis(500), now_i) {
                    let now = current_screen_key(&app, gate.pref());
                    if now.is_some() && now != last_screen {
                        let first = last_screen.is_none();
                        last_screen = now;
                        if !first {
                            let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                        }
                    }
                }
                if due(&mut fs_at, Duration::from_millis(250), now_i) {
                    fs = gate.hide_fullscreen.load(Ordering::Relaxed)
                        && sys::fullscreen_app_active();
                }

                let now = now_ms();
                let revealed = now < gate.reveal_until_ms.load(Ordering::Relaxed);
                let quiet = fs && !revealed;
                if quiet_sent != Some(quiet) {
                    quiet_sent = Some(quiet);
                    let _ = app.emit_to(WINDOW_LABEL, "quiet", quiet);
                    if quiet && hint_on {
                        hint_on = false;
                        let _ = app.emit_to(WINDOW_LABEL, "drag-hint", false);
                    }
                }

                let Some(win) = window(&app) else {
                    was_down = down;
                    continue;
                };

                // Idle fast path: the strip is collapsed, nothing is held. Keep it
                // click-through and go back to sleep.
                if collapsed && !down && !was_down && !hint_on {
                    if !gate.ignoring.load(Ordering::Relaxed) {
                        gate.ignoring.store(true, Ordering::Relaxed);
                        let _ = win.set_ignore_cursor_events(true);
                    }
                    was_space = false;
                    last_collapsed = collapsed;
                    continue;
                }

                let Some((cx, cy)) = sys::cursor_physical() else {
                    was_down = down;
                    continue;
                };

                // Window origin / scale / size change rarely and each query is a round
                // trip to the UI thread: refresh a few times a second, or right after a
                // resize.
                if geo.is_none()
                    || collapsed != last_collapsed
                    || due(&mut geo_at, Duration::from_millis(150), now_i)
                {
                    last_collapsed = collapsed;
                    geo_at = Some(now_i);
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
                    was_down = down;
                    continue;
                };
                let x = (cx - ox) / scale;
                let y = (cy - oy) / scale;
                let mid = ox + w * scale / 2.0;
                let inside = x >= 0.0 && x <= w && y >= 0.0 && y <= h;

                // Keep the bar available while the cursor is around it in a full-screen app.
                if fs
                    && revealed
                    && (-4.0..=PANEL_H + 40.0).contains(&y)
                    && (cx - mid).abs() <= (PANEL_W / 2.0) * scale
                {
                    gate.reveal_until_ms
                        .store(now + REVEAL_MS / 2, Ordering::Relaxed);
                }

                // ---- Space while holding a file ---------------------------------------
                // Picking a file up does nothing on its own. Pressing Space while it is
                // held opens the drop view; letting go anywhere closes it again.
                let space = down && sys::space_down();
                if space && !was_space && !quiet && is_file_surface(&sys::foreground_class()) {
                    hint_on = !hint_on;
                    let _ = app.emit_to(WINDOW_LABEL, "drag-hint", hint_on);
                }
                was_space = space;
                if !down && hint_on {
                    hint_on = false;
                    let _ = app.emit_to(WINDOW_LABEL, "drag-hint", false);
                }
                was_down = down;

                // ---- click-through and cursor feed -------------------------------------
                let r = *gate.rect.lock().unwrap();
                let key = (r.x, r.y, r.w, r.h, hint_on, collapsed);
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
                // A file being dragged has to find a drop target, and click-through hides
                // the window from OLE: only while the drag hint is up does the whole
                // window take the mouse. At any other time a drag passes straight through
                // (desktop icons, window borders, ...).
                let accept = on_island || (hint_on && inside && !quiet);
                if gate.ignoring.load(Ordering::Relaxed) == accept {
                    gate.ignoring.store(!accept, Ordering::Relaxed);
                    let _ = win.set_ignore_cursor_events(!accept);
                }

                // The page only needs the cursor while the panel is open and the cursor is
                // near it (Bob's eyes follow it, hover timers need to know when it left).
                let near = !collapsed
                    && !quiet
                    && (-220.0..=w + 220.0).contains(&x)
                    && (-60.0..=h + 220.0).contains(&y);
                if (near || was_near)
                    && (on_island || last_feed.elapsed() >= Duration::from_millis(33))
                {
                    last_feed = Instant::now();
                    let _ = app.emit_to(WINDOW_LABEL, "cursor", CursorPayload { x, y });
                }
                was_near = near;
            }
        }
    });
}
