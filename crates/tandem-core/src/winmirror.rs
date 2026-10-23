//! Windows side of second-monitor mode (see `mirror.rs` for the idea).
//!
//! The owner keeps the real windows: parked past the edge, slid onto the screen while
//! the pointer is on their mirror, and photographed a few dozen times a second.
//!
//! The mirror end shows each one in an ordinary window of its own that other windows
//! can cover; the hook notices the pointer going onto it.

use crate::mirror::{self, Rect};
use crate::platform::CaptureEvent;
use crate::proto::Edge;
use crate::span;
use crate::win::{emit, hide_pointer, vscreen};
use crate::winspan::{self, grab_pixels, moving_window_any, visible_rect, window_rect};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicIsize, AtomicU64, AtomicUsize, Ordering::Relaxed,
};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::*;

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

fn buttons_down() -> bool {
    unsafe {
        GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 || GetAsyncKeyState(VK_RBUTTON.0 as i32) < 0
    }
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

fn rect_of(r: RECT) -> Rect {
    Rect {
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    }
}

// ================================================================== the owner

struct Owned {
    hwnd: isize,
    /// The edge it is parked past (the one leading to the computer showing it).
    edge: Edge,
    was_topmost: bool,
    /// The visible frame minus the full rectangle (invisible resize borders).
    off: (i32, i32),
    /// Along the edge, where it waits (the visible part's top-left when parked).
    parked: (i32, i32),
    /// The monitor it slides onto for the pointer.
    work: Rect,
    /// Slid in for the pointer: where (the visible part).
    slid: Option<Rect>,
    /// Where it was last seen (visible top-left), to notice the person moving it.
    seen: (i32, i32),
    /// It is being dragged by its title bar (through the mirror).
    dragging: bool,
    /// Just adopted: its first picture goes out even if it looks like the last one.
    fresh: bool,
}

static OWNED: Mutex<Vec<Owned>> = Mutex::new(Vec::new());
/// The window the pointer is on, for the hook: id, and its visible rectangle.
static PORTAL_ID: AtomicU64 = AtomicU64::new(0);
static PORTAL_ON: AtomicBool = AtomicBool::new(false);
static PR: [AtomicI32; 4] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];
static OWNER_THREADS: Once = Once::new();

fn set_portal_rect(r: Rect) {
    PR[0].store(r.x, Relaxed);
    PR[1].store(r.y, Relaxed);
    PR[2].store(r.w, Relaxed);
    PR[3].store(r.h, Relaxed);
}

fn portal_rect() -> Rect {
    Rect {
        x: PR[0].load(Relaxed),
        y: PR[1].load(Relaxed),
        w: PR[2].load(Relaxed),
        h: PR[3].load(Relaxed),
    }
}

/// Moves window `h` so its visible part's top-left is at (`x`, `y`). `top`: keep it
/// above other windows (and make sure it shows).
fn place(h: isize, off: (i32, i32), x: i32, y: i32, top: bool) {
    unsafe {
        let _ = SetWindowPos(
            hwnd(h),
            Some(if top { HWND_TOPMOST } else { HWND_NOTOPMOST }),
            x - off.0,
            y - off.1,
            0,
            0,
            // Never wait for the program: a hung one must not hold up the mouse.
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// Like `place` with `top`, but just under the veil while it is up (the window must not
/// be seen on this screen while it is used through the mirror).
fn place_for_pointer(h: isize, off: (i32, i32), x: i32, y: i32) {
    match winspan::veil_window() {
        Some(v) => unsafe {
            let _ = SetWindowPos(
                hwnd(h),
                Some(hwnd(v)),
                x - off.0,
                y - off.1,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
            );
        },
        None => place(h, off, x, y, true),
    }
}

/// The pointer is done with a window slid in: the veil goes, and the pointer shows again
/// (unless the mouse is driving the other computer, which keeps it hidden).
fn portal_over() {
    winspan::veil(None);
    crate::win::show_pointer_unless_capturing();
}

fn work_area(h: isize) -> Rect {
    unsafe {
        let mon = MonitorFromWindow(hwnd(h), MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            return rect_of(mi.rcWork);
        }
    }
    let s = vscreen();
    Rect {
        x: s.x,
        y: s.y,
        w: s.width,
        h: s.height,
    }
}

fn park(o: &mut Owned) {
    let Some(vis) = visible_rect(o.hwnd) else {
        return;
    };
    let v = rect_of(vis);
    let (x, y) = mirror::park_at(vscreen(), o.edge, v.w, v.h, o.parked);
    place(o.hwnd, o.off, x, y, true);
    o.parked = (x, y);
    o.seen = (x, y);
    o.slid = None;
}

/// The window just let go over `edge` stays here, parked past it.
pub fn adopt(edge: Edge) -> Option<(u64, u32, u32)> {
    let h = winspan::take_last()?;
    unsafe {
        if !IsWindow(Some(hwnd(h))).as_bool() {
            return None;
        }
        if IsZoomed(hwnd(h)).as_bool() {
            let _ = ShowWindow(hwnd(h), SW_RESTORE);
        }
    }
    let work = work_area(h);
    let (full, vis) = (window_rect(h)?, visible_rect(h)?);
    let off = (vis.left - full.left, vis.top - full.top);
    let mut v = rect_of(vis);
    // It has to fit on this screen to be driven here: shrink it if it doesn't.
    let fit = mirror::slide_in(work, v.w, v.h);
    if (fit.w, fit.h) != (v.w, v.h) {
        let (bw, bh) = (
            (full.right - full.left) - v.w,
            (full.bottom - full.top) - v.h,
        );
        unsafe {
            let _ = SetWindowPos(
                hwnd(h),
                None,
                0,
                0,
                fit.w + bw,
                fit.h + bh,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        v.w = fit.w;
        v.h = fit.h;
    }
    let was_topmost = unsafe { GetWindowLongW(hwnd(h), GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0 };
    let mut o = Owned {
        hwnd: h,
        edge,
        was_topmost,
        off,
        parked: (v.x, v.y),
        work,
        slid: None,
        seen: (v.x, v.y),
        dragging: false,
        fresh: true,
    };
    park(&mut o);
    let id = h as u64;
    if let Ok(mut all) = OWNED.lock() {
        all.retain(|x| x.hwnd != h);
        all.push(o);
    }
    OWNER_THREADS.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-mirror-pics".into())
            .spawn(pictures);
        let _ = std::thread::Builder::new()
            .name("tandem-mirror-cursor".into())
            .spawn(cursor_watch);
    });
    log::info!(
        "mirror: keeping window {id} here ({}x{}), parked past the {edge:?} edge",
        v.w,
        v.h
    );
    Some((id, v.w as u32, v.h as u32))
}

/// Stop mirroring `id` and put the window back on this screen.
pub fn release(id: u64, home: Option<f32>) {
    let Some(o) = OWNED.lock().ok().and_then(|mut all| {
        let i = all.iter().position(|o| o.hwnd as u64 == id)?;
        Some(all.remove(i))
    }) else {
        return;
    };
    let was_portal = PORTAL_ON.load(Relaxed) && PORTAL_ID.load(Relaxed) == id;
    if was_portal {
        PORTAL_ON.store(false, Relaxed);
        portal_over();
    }
    let Some(vis) = visible_rect(o.hwnd).map(rect_of) else {
        return;
    };
    let (x, y) = match home {
        Some(along) => mirror::home_at(vscreen(), o.edge, along, vis.w, vis.h),
        None => {
            let r = mirror::slide_in(o.work, vis.w, vis.h);
            (r.x, r.y)
        }
    };
    place(o.hwnd, o.off, x, y, o.was_topmost);
    if was_portal {
        // The pointer stays on the same spot of the window.
        let (cx, cy) = cursor();
        unsafe {
            let _ = SetCursorPos(cx + x - vis.x, cy + y - vis.y);
        }
    }
}

/// Every window still kept here goes back on screen (CubeCursor is closing).
pub fn release_all() {
    let ids: Vec<u64> = OWNED
        .lock()
        .map(|all| all.iter().map(|o| o.hwnd as u64).collect())
        .unwrap_or_default();
    for id in ids {
        release(id, None);
    }
}

/// The pointer comes onto window `id` at (`rx`, `ry`): slide it in.
pub fn portal_begin(id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
    let mut all = OWNED.lock().ok()?;
    let o = all.iter_mut().find(|o| o.hwnd as u64 == id)?;
    unsafe {
        if !IsWindow(Some(hwnd(o.hwnd))).as_bool() {
            return None;
        }
        if IsIconic(hwnd(o.hwnd)).as_bool() {
            let _ = ShowWindow(hwnd(o.hwnd), SW_RESTORE);
        }
    }
    let v = rect_of(visible_rect(o.hwnd)?);
    let r = mirror::slide_in(o.work, v.w, v.h);
    // A picture of the screen as it is goes over that spot first, then the window slides
    // in under it: on this screen nothing seems to change.
    match winspan::screen_pixels(r.x, r.y, r.w, r.h) {
        Some(px) => winspan::veil(Some(winspan::Veil {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
            px,
            then: Some((o.hwnd, o.off, r.x, r.y)),
        })),
        None => place(o.hwnd, o.off, r.x, r.y, true),
    }
    o.slid = Some(r);
    o.seen = (r.x, r.y);
    o.dragging = false;
    set_portal_rect(r);
    PORTAL_ID.store(id, Relaxed);
    PORTAL_ON.store(true, Relaxed);
    Some((r.x + rx.clamp(0, r.w - 1), r.y + ry.clamp(0, r.h - 1)))
}

/// The pointer left the window: park it again.
pub fn portal_end() {
    if !PORTAL_ON.swap(false, Relaxed) {
        return;
    }
    portal_over();
    let id = PORTAL_ID.load(Relaxed);
    if let Ok(mut all) = OWNED.lock() {
        if let Some(o) = all.iter_mut().find(|o| o.hwnd as u64 == id) {
            park(o);
        }
    }
}

/// Driven from the other computer: is the pointer off the window (no button down)?
pub fn portal_outside() -> Option<(i32, i32)> {
    if !PORTAL_ON.load(Relaxed) || buttons_down() {
        return None;
    }
    let r = portal_rect();
    let (x, y) = cursor();
    (!r.contains(x, y)).then_some((x - r.x, y - r.y))
}

/// What the hook does with a move of this computer's own mouse.
pub enum Hook {
    /// Nothing to do with mirrors: carry on (edges and all).
    Pass,
    /// On a window being driven here: no edges, nothing else.
    Stay,
    /// Swallow from now on and send this.
    Cross(CaptureEvent),
}

pub fn hook_move(x: i32, y: i32, left_held: bool) -> Hook {
    if PORTAL_ON.load(Relaxed) {
        let r = portal_rect();
        if left_held || r.contains(x, y) || buttons_down() {
            return Hook::Stay;
        }
        return Hook::Cross(CaptureEvent::PortalOut {
            rx: x - r.x,
            ry: y - r.y,
        });
    }
    if MIRROR_COUNT.load(Relaxed) == 0 || left_held || winspan::showing() {
        return Hook::Pass;
    }
    match mirror_under(x, y) {
        Some((id, r)) if !buttons_down() => {
            post(Cmd::Raise(id));
            Hook::Cross(CaptureEvent::PortalIn {
                id,
                rx: x - r.x,
                ry: y - r.y,
            })
        }
        _ => Hook::Pass,
    }
}

/// Pictures of every window kept here, and an eye on what happens to them.
fn pictures() {
    let mut prints: HashMap<isize, u64> = HashMap::new();
    loop {
        let t = Instant::now();
        let hs: Vec<isize> = OWNED
            .lock()
            .map(|all| all.iter().map(|o| o.hwnd).collect())
            .unwrap_or_default();
        for h in hs {
            look_after(h, &mut prints);
        }
        let left = Duration::from_millis(mirror::FRAME_EVERY_MS).saturating_sub(t.elapsed());
        std::thread::sleep(left.max(Duration::from_millis(8)));
    }
}

fn look_after(h: isize, prints: &mut HashMap<isize, u64>) {
    let id = h as u64;
    let alive = unsafe { IsWindow(Some(hwnd(h))).as_bool() };
    if !alive {
        if let Ok(mut all) = OWNED.lock() {
            all.retain(|o| o.hwnd != h);
        }
        if PORTAL_ID.load(Relaxed) == id && PORTAL_ON.swap(false, Relaxed) {
            portal_over();
        }
        prints.remove(&h);
        emit(CaptureEvent::MirrorGone { id });
        return;
    }
    if unsafe { IsIconic(hwnd(h)).as_bool() } {
        return;
    }
    // Snapped or maximised on this screen (dragged to its top): put it back.
    if unsafe { IsZoomed(hwnd(h)).as_bool() } {
        unsafe {
            let _ = ShowWindow(hwnd(h), SW_RESTORE);
        }
    }
    let Some(vis) = visible_rect(h).map(rect_of) else {
        return;
    };
    let Some(full) = window_rect(h) else {
        return;
    };
    // Moved by the person (through the mirror), or by anything else.
    if let Ok(mut all) = OWNED.lock() {
        if let Some(o) = all.iter_mut().find(|o| o.hwnd == h) {
            if std::mem::take(&mut o.fresh) {
                prints.remove(&h);
            }
            o.off = (vis.x - full.left, vis.y - full.top);
            let moving = moving_window_any() == Some(h);
            match o.slid {
                Some(_) => {
                    let (dx, dy) = (vis.x - o.seen.0, vis.y - o.seen.1);
                    if moving && (dx != 0 || dy != 0) {
                        o.dragging = true;
                        emit(CaptureEvent::MirrorMoved {
                            id,
                            dx,
                            dy,
                            done: false,
                        });
                    }
                    o.seen = (vis.x, vis.y);
                    if o.dragging && !moving {
                        // Let go: the mirror stays where it went; the real window goes
                        // back to the middle, and the pointer with it.
                        o.dragging = false;
                        emit(CaptureEvent::MirrorMoved {
                            id,
                            dx: 0,
                            dy: 0,
                            done: true,
                        });
                        let r = mirror::slide_in(o.work, vis.w, vis.h);
                        place_for_pointer(h, o.off, r.x, r.y);
                        let (cx, cy) = cursor();
                        unsafe {
                            let _ = SetCursorPos(cx + r.x - vis.x, cy + r.y - vis.y);
                        }
                        o.slid = Some(r);
                        o.seen = (r.x, r.y);
                        if PORTAL_ID.load(Relaxed) == id {
                            set_portal_rect(r);
                        }
                    } else if PORTAL_ID.load(Relaxed) == id {
                        // Moved or resized: the hook needs where it is now.
                        o.slid = Some(vis);
                        set_portal_rect(vis);
                    }
                }
                None => {
                    if !moving && (vis.x, vis.y) != o.parked {
                        park(o);
                    }
                }
            }
        }
    }
    let Some((px, cw, ch)) = grab_pixels(h, (vis.x - full.left, vis.y - full.top), vis.w, vis.h)
    else {
        return;
    };
    let fp = mirror::fingerprint(&px);
    if prints.get(&h) == Some(&fp) {
        return;
    }
    prints.insert(h, fp);
    let (sw, sh) = span::fit(cw, ch, mirror::FRAME_MAX_W, mirror::FRAME_MAX_H);
    let small = if (sw, sh) == (cw, ch) {
        px
    } else {
        span::scale4(&px, cw, ch, sw, sh)
    };
    if let Some(jpeg) = span::encode_jpeg(&small, sw, sh, mirror::FRAME_QUALITY) {
        emit(CaptureEvent::MirrorFrame {
            id,
            w: cw,
            h: ch,
            jpeg,
        });
    }
}

/// While the pointer is on a window driven here: where it is on it, for the mirror.
fn cursor_watch() {
    let mut last = (i32::MIN, i32::MIN, 0u64);
    loop {
        std::thread::sleep(Duration::from_millis(mirror::CURSOR_EVERY_MS));
        if !PORTAL_ON.load(Relaxed) {
            last = (i32::MIN, i32::MIN, 0);
            continue;
        }
        let id = PORTAL_ID.load(Relaxed);
        let r = portal_rect();
        let (x, y) = cursor();
        let now = (x - r.x, y - r.y, id);
        if now != last {
            last = now;
            emit(CaptureEvent::PortalCursor {
                id,
                rx: now.0,
                ry: now.1,
            });
        }
    }
}

// ================================================================== the mirror end

struct Shown {
    id: u64,
    hwnd: isize,
    rect: Rect,
    entry: Edge,
}

static SHOWN: Mutex<Vec<Shown>> = Mutex::new(Vec::new());
static MIRROR_COUNT: AtomicUsize = AtomicUsize::new(0);
/// The pictures, painted by the window procedure (id → BGRA, width, height).
/// A picture: BGRA, width, height.
pub(crate) type Pic = (Vec<u8>, u32, u32);
static PICS: Mutex<Vec<(u64, Pic)>> = Mutex::new(Vec::new());

enum Cmd {
    Create(u64, Rect),
    Frame(u64, u32, u32, Vec<u8>),
    Move(u64, Rect),
    Raise(u64),
    /// The mirror's window (0 if it was not made yet).
    Close(u64, isize),
}

static QUEUE: Mutex<VecDeque<Cmd>> = Mutex::new(VecDeque::new());
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
static UI_START: Once = Once::new();
const WM_MIRROR: u32 = WM_APP + 0x52;

fn post(c: Cmd) {
    UI_START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-mirror-ui".into())
            .spawn(ui_thread);
    });
    if let Ok(mut q) = QUEUE.lock() {
        // A newer picture of the same window makes an older one pointless.
        if let Cmd::Frame(id, ..) = &c {
            let id = *id;
            q.retain(|old| !matches!(old, Cmd::Frame(i, ..) if *i == id));
        }
        q.push_back(c);
    }
    let h = UI_HWND.load(Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(hwnd(h)), WM_MIRROR, WPARAM(0), LPARAM(0));
        }
    }
}

fn mirror_under(x: i32, y: i32) -> Option<(u64, Rect)> {
    let root = unsafe {
        let w = WindowFromPoint(POINT { x, y });
        if w.0.is_null() {
            return None;
        }
        GetAncestor(w, GA_ROOT).0 as isize
    };
    let shown = SHOWN.try_lock().ok()?;
    shown
        .iter()
        .find(|s| s.hwnd == root && s.rect.contains(x, y))
        .map(|s| (s.id, s.rect))
}

pub fn create(id: u64, w: u32, h: u32, entry: Edge) -> bool {
    let s = vscreen();
    let (px, py, pic) = winspan::current_picture().unwrap_or((
        s.x + (s.width - w as i32) / 2,
        s.y + (s.height - h as i32) / 2,
        None,
    ));
    let rect = mirror::first_place(
        Rect {
            x: px,
            y: py,
            w: w as i32,
            h: h as i32,
        },
        s,
    );
    if let Ok(mut shown) = SHOWN.lock() {
        shown.retain(|m| m.id != id);
        shown.push(Shown {
            id,
            hwnd: 0,
            rect,
            entry,
        });
        MIRROR_COUNT.store(shown.len(), Relaxed);
    }
    if let Some((px, fw, fh)) = pic {
        set_pic(id, px, fw, fh);
    }
    post(Cmd::Create(id, rect));
    // The picture of it being dragged across is replaced by the mirror.
    winspan::hide(0);
    true
}

fn set_pic(id: u64, px: Vec<u8>, w: u32, h: u32) {
    if let Ok(mut p) = PICS.lock() {
        p.retain(|x| x.0 != id);
        p.push((id, (px, w, h)));
    }
}

pub fn frame(id: u64, w: u32, h: u32, jpeg: Vec<u8>) {
    post(Cmd::Frame(id, w, h, jpeg));
}

pub fn close(id: u64) {
    let mut hw = 0;
    if let Ok(mut shown) = SHOWN.lock() {
        if let Some(m) = shown.iter().find(|m| m.id == id) {
            hw = m.hwnd;
        }
        shown.retain(|m| m.id != id);
        MIRROR_COUNT.store(shown.len(), Relaxed);
    }
    post(Cmd::Close(id, hw));
}

pub fn move_by(id: u64, dx: i32, dy: i32, done: bool) -> Option<f32> {
    let (rect, entry) = {
        let mut shown = SHOWN.lock().ok()?;
        let m = shown.iter_mut().find(|m| m.id == id)?;
        m.rect.x += dx;
        m.rect.y += dy;
        (m.rect, m.entry)
    };
    post(Cmd::Move(id, rect));
    if !done {
        return None;
    }
    match mirror::home_along(rect, vscreen(), entry) {
        Some(along) => {
            close(id);
            Some(along)
        }
        None => {
            // Let go anywhere else: it stays where it can be reached, never off screen.
            let kept = mirror::first_place(rect, vscreen());
            if kept != rect {
                if let Ok(mut shown) = SHOWN.lock() {
                    if let Some(m) = shown.iter_mut().find(|m| m.id == id) {
                        m.rect = kept;
                    }
                }
                post(Cmd::Move(id, kept));
            }
            None
        }
    }
}

pub fn at_cursor() -> Option<(u64, i32, i32)> {
    let (x, y) = cursor();
    if buttons_down() {
        return None;
    }
    let (id, r) = mirror_under(x, y)?;
    post(Cmd::Raise(id));
    Some((id, x - r.x, y - r.y))
}

pub fn point(id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
    let shown = SHOWN.lock().ok()?;
    let m = shown.iter().find(|m| m.id == id)?;
    Some((m.rect.x + rx, m.rect.y + ry))
}

pub fn cursor_on(id: u64, at: Option<(i32, i32)>) {
    match at.and_then(|(x, y)| point(id, x, y)) {
        Some(p) => winspan::cursor_overlay(Some(p)),
        None => winspan::cursor_overlay(None),
    }
}

pub fn pointer_hidden(on: bool) {
    hide_pointer(on);
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MIRROR => {
            let _ = std::panic::catch_unwind(work);
            LRESULT(0)
        }
        WM_PAINT => {
            let _ = std::panic::catch_unwind(|| paint(h));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
        WM_CLOSE => {
            // Closed from its taskbar button: the window goes back to its computer.
            let id = SHOWN
                .lock()
                .ok()
                .and_then(|s| s.iter().find(|m| m.hwnd == h.0 as isize).map(|m| m.id));
            if let Some(id) = id {
                emit(CaptureEvent::MirrorDismissed { id });
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

fn paint(h: HWND) {
    let id = SHOWN
        .lock()
        .ok()
        .and_then(|s| s.iter().find(|m| m.hwnd == h.0 as isize).map(|m| m.id));
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(h, &mut rc);
        let pics = PICS.lock().ok();
        let pic = pics
            .as_ref()
            .and_then(|p| p.iter().find(|x| Some(x.0) == id));
        match pic {
            Some((_, (px, w, ht))) => {
                let bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: *w as i32,
                        biHeight: -(*ht as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                SetStretchBltMode(dc, HALFTONE);
                StretchDIBits(
                    dc,
                    0,
                    0,
                    rc.right,
                    rc.bottom,
                    0,
                    0,
                    *w as i32,
                    *ht as i32,
                    Some(px.as_ptr() as *const core::ffi::c_void),
                    &bi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
            None => {
                let b = CreateSolidBrush(COLORREF(0x0020_1a18));
                FillRect(dc, &rc, b);
                let _ = DeleteObject(b.into());
            }
        }
        let _ = EndPaint(h, &ps);
    }
}

fn ui_thread() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let hinst = HINSTANCE(module.0);
        let wc = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("TandemMirror"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        // An invisible window that only receives the commands.
        let ctl = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("TandemMirror"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        )
        .map(|h| h.0 as isize)
        .unwrap_or(0);
        if ctl == 0 {
            log::warn!("mirror: cannot create the mirror window");
            return;
        }
        UI_HWND.store(ctl, Relaxed);
        work();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn hwnd_of(id: u64) -> Option<isize> {
    SHOWN
        .lock()
        .ok()?
        .iter()
        .find(|m| m.id == id && m.hwnd != 0)
        .map(|m| m.hwnd)
}

/// Runs on the mirror thread: applies what was queued.
fn work() {
    let cmds: Vec<Cmd> = QUEUE
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    for c in cmds {
        match c {
            Cmd::Create(id, r) => unsafe {
                let Ok(module) = GetModuleHandleW(None) else {
                    continue;
                };
                // A taskbar button of its own: covered by other windows, it can always be
                // brought back to the front (or closed, which sends the window home).
                let made = CreateWindowExW(
                    WS_EX_APPWINDOW,
                    w!("TandemMirror"),
                    w!("Window from the other computer (CubeCursor)"),
                    WS_POPUP | WS_VISIBLE | WS_SYSMENU | WS_MINIMIZEBOX,
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    None,
                    None,
                    Some(HINSTANCE(module.0)),
                    None,
                );
                if let Ok(h) = made {
                    let mut gone = false;
                    if let Ok(mut shown) = SHOWN.lock() {
                        match shown.iter_mut().find(|m| m.id == id) {
                            Some(m) => m.hwnd = h.0 as isize,
                            None => gone = true, // closed while it was being made
                        }
                    }
                    if gone {
                        let _ = DestroyWindow(h);
                    } else {
                        let _ = SetWindowPos(
                            h,
                            Some(HWND_TOP),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            },
            Cmd::Frame(id, w, h, jpeg) => {
                let Some((px, fw, fh)) = span::decode_jpeg(&jpeg) else {
                    continue;
                };
                set_pic(id, px, fw, fh);
                // The window changed size over there: so does the mirror.
                let resized = SHOWN.lock().ok().and_then(|mut shown| {
                    let m = shown.iter_mut().find(|m| m.id == id)?;
                    let changed = (m.rect.w, m.rect.h) != (w as i32, h as i32);
                    m.rect.w = w as i32;
                    m.rect.h = h as i32;
                    changed.then_some(m.rect)
                });
                if let Some(hw) = hwnd_of(id) {
                    unsafe {
                        if let Some(r) = resized {
                            let _ = SetWindowPos(
                                hwnd(hw),
                                None,
                                0,
                                0,
                                r.w,
                                r.h,
                                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                        }
                        let _ = InvalidateRect(Some(hwnd(hw)), None, false);
                    }
                }
            }
            Cmd::Move(id, r) => {
                if let Some(hw) = hwnd_of(id) {
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd(hw),
                            None,
                            r.x,
                            r.y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
            Cmd::Raise(id) => {
                if let Some(hw) = hwnd_of(id) {
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd(hw),
                            Some(HWND_TOP),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            }
            Cmd::Close(id, hw) => {
                if let Ok(mut p) = PICS.lock() {
                    p.retain(|x| x.0 != id);
                }
                if hw != 0 {
                    unsafe {
                        let _ = DestroyWindow(hwnd(hw));
                    }
                }
            }
        }
    }
}
