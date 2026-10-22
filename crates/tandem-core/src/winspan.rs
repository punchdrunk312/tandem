//! Windows side of dragging a window across the edge (see `span.rs`).
//!
//! Two phases, for any window Windows is moving by its title bar:
//!
//! * Preview: Windows still moves the window. As soon as part of it goes past an edge
//!   that leads to another computer, that computer starts showing the part that is over.
//! * Driven: the pointer reaches the edge. Windows' own move is stopped (Esc, which also
//!   drops the snap preview) and this computer moves the window itself, letting it hang
//!   as far over as you push it. The real pointer is hidden and a stand-in arrow follows.
//!
//! The mouse driving it is either this computer's own (its hooks feed the moves) or the
//! other computer's while it controls this one ("remote": the engine feeds the moves that
//! arrive over the network). A thread takes pictures of the window (`PrintWindow` also
//! draws the part that is off-screen) and hands them to the engine as JPEG.
//!
//! Receiving: a click-through, never-focused, always-on-top layered window shows the
//! picture where the window would be on this screen.
//!
//! Everything the mouse hook calls only touches atomics, a mutex nobody holds for long,
//! and non-blocking window calls; the waiting happens on threads of its own.

use crate::platform::{CaptureEvent, Screen, SpanView};
use crate::proto::Edge;
use crate::span::{self, Geo};
use crate::win::{
    emit, hide_pointer, key_input, mouse_input, send, set_capturing, span_app, vscreen,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{MOUSEEVENTF_LEFTUP, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

// ------------------------------------------------------------------ sending

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Preview,
    Driven,
}

struct Out {
    geo: Geo,
    hwnd: isize,
    /// The window's visible frame minus its full rectangle (Windows 10/11 windows have
    /// invisible borders for resizing; the picture and the maths use the visible part).
    off_x: i32,
    off_y: i32,
    /// The pointer as if the screen went on past the edge.
    vx: i32,
    vy: i32,
    phase: Phase,
    /// Driven by the other computer's mouse (this one is being controlled).
    remote: bool,
    /// Windows' own move has been stopped: from now on this computer moves the window.
    settled: bool,
    last_sent: Instant,
}

static OUT: Mutex<Option<Out>> = Mutex::new(None);
/// The driven phase is on: the hooks route the mouse here (read without locking).
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
/// A preview or a drive is on.
static SHOWING: AtomicBool = AtomicBool::new(false);
/// Bumped whenever a drag across ends, so its picture thread stops.
static GEN: AtomicU32 = AtomicU32::new(0);
/// When the moving window was last looked at (the hook sees ~1000 moves a second).
static LAST_WATCH_US: AtomicU64 = AtomicU64::new(0);
/// The last window that hung across, to put it back if the program didn't open there.
static LAST: Mutex<Option<(isize, Screen)>> = Mutex::new(None);

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

fn now_us() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_micros() as u64
}

fn window_rect(h: isize) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd(h), &mut r).ok()? };
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// The part of the window you can see.
fn visible_rect(h: isize) -> Option<RECT> {
    let mut r = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd(h),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
    };
    if ok && r.right > r.left && r.bottom > r.top {
        Some(r)
    } else {
        window_rect(h)
    }
}

/// The window Windows is moving by its title bar right now, if any (any program).
pub fn moving_window() -> Option<isize> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        if tid == 0 {
            return None;
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(tid, &mut info).ok()?;
        if !info.flags.contains(GUI_INMOVESIZE) || info.hwndMoveSize.0.is_null() {
            return None;
        }
        let root = GetAncestor(info.hwndMoveSize, GA_ROOT);
        (!root.0.is_null()).then_some(root.0 as isize)
    }
}

/// A new drag across for window `h` held at `pt`, if part of it is past one of `edges`
/// (or, with `at_edge`, the pointer itself is at that edge).
fn start(
    h: isize,
    edges: &[Edge],
    pt: (i32, i32),
    scr: Screen,
    remote: bool,
    at_edge: Option<Edge>,
) -> Option<Out> {
    let full = window_rect(h)?;
    let vis = visible_rect(h)?;
    let (w, ht) = (vis.right - vis.left, vis.bottom - vis.top);
    if w < 40 || ht < 30 {
        return None;
    }
    let geo_for = |edge: Edge| Geo {
        edge,
        scr,
        w,
        h: ht,
        gx: (pt.0 - vis.left).clamp(0, w - 1),
        gy: (pt.1 - vis.top).clamp(0, ht - 1),
    };
    let edge = match at_edge {
        Some(e) => e,
        None => *edges
            .iter()
            .find(|e| !e.is_corner() && geo_for(**e).crossed(pt.0, pt.1) > 0)?,
    };
    Some(Out {
        geo: geo_for(edge),
        hwnd: h,
        off_x: vis.left - full.left,
        off_y: vis.top - full.top,
        vx: pt.0,
        vy: pt.1,
        phase: Phase::Preview,
        remote,
        settled: false,
        last_sent: Instant::now() - Duration::from_secs(1),
    })
}

/// Begins showing `o` on the other computer and starts its pictures.
fn announce(o: &mut Out) {
    let gen = GEN.fetch_add(1, Relaxed) + 1;
    SHOWING.store(true, Relaxed);
    log::info!(
        "app span: {}x{} window crosses the {:?} edge{}",
        o.geo.w,
        o.geo.h,
        o.geo.edge,
        if o.remote {
            " (driven from the other computer)"
        } else {
            ""
        }
    );
    emit(CaptureEvent::SpanBegin {
        edge: o.geo.edge,
        w: o.geo.w as u32,
        h: o.geo.h as u32,
    });
    report(o, true);
    let (h, off, w, ht) = (o.hwnd, (o.off_x, o.off_y), o.geo.w, o.geo.h);
    let _ = std::thread::Builder::new()
        .name("tandem-span-pics".into())
        .spawn(move || pictures(gen, h, off, w, ht));
}

/// Ends the drag across without moving anything: the other computer stops showing it.
fn finish(o: &Out) {
    GEN.fetch_add(1, Relaxed);
    SHOWING.store(false, Relaxed);
    ACTIVE.store(false, Relaxed);
    if let Ok(mut l) = LAST.lock() {
        *l = Some((o.hwnd, o.geo.scr));
    }
}

/// While a window is being moved: show it on the other computer as soon as part of it
/// is past one of `edges`, and stop as soon as it is all back. `pt` is the pointer.
pub fn watch(edges: &[Edge], pt: (i32, i32), scr: Screen, remote: bool) {
    if ACTIVE.load(Relaxed) {
        return;
    }
    let now = now_us();
    if now.saturating_sub(LAST_WATCH_US.load(Relaxed)) < 12_000 {
        return;
    }
    LAST_WATCH_US.store(now, Relaxed);
    let Ok(mut g) = OUT.try_lock() else {
        return;
    };
    let moving = moving_window();
    if let Some(o) = g.as_mut() {
        if o.phase == Phase::Driven {
            return;
        }
        let still = moving == Some(o.hwnd) && {
            o.vx = pt.0;
            o.vy = pt.1;
            o.geo.crossed(pt.0, pt.1) > 0
        };
        if still {
            report(o, false);
            return;
        }
        // Back on this screen (or no longer being moved): the other computer lets go.
        let o = g.take().unwrap();
        finish(&o);
        emit(CaptureEvent::SpanEnd {
            moved: false,
            app: None,
            frac: 0.0,
        });
    }
    let Some(h) = moving else {
        return;
    };
    if let Some(mut o) = start(h, edges, pt, scr, remote, None) {
        announce(&mut o);
        *g = Some(o);
    }
}

/// The pointer reached `edge` while a window is held: from here this computer moves the
/// window. True if a drag across is on now (the caller swallows the move).
pub fn take(edge: Edge, pt: (i32, i32), scr: Screen, remote: bool) -> bool {
    if edge.is_corner() || ACTIVE.load(Relaxed) {
        return false;
    }
    let Ok(mut g) = OUT.lock() else {
        return false;
    };
    let moving = moving_window();
    let fresh = match g.as_ref() {
        Some(o) => o.geo.edge != edge || Some(o.hwnd) != moving,
        None => true,
    };
    if fresh {
        if let Some(o) = g.take() {
            finish(&o);
            emit(CaptureEvent::SpanEnd {
                moved: false,
                app: None,
                frac: 0.0,
            });
        }
        let Some(h) = moving else {
            return false;
        };
        let Some(mut o) = start(h, &[edge], pt, scr, remote, Some(edge)) else {
            return false;
        };
        announce(&mut o);
        *g = Some(o);
    }
    let o = g.as_mut().unwrap();
    o.phase = Phase::Driven;
    o.vx = pt.0;
    o.vy = pt.1;
    o.settled = false;
    ACTIVE.store(true, Relaxed);
    if remote {
        hide_pointer(true);
    } else {
        // Swallow the mouse from now on and hide the real pointer (parked mid-screen).
        set_capturing(true);
        let (cx, cy) = scr.center();
        unsafe {
            let _ = SetCursorPos(cx, cy);
        }
    }
    cursor_overlay(Some(pt));
    log::info!("app span: the pointer reached the edge: CubeCursor moves the window now");
    let _ = std::thread::Builder::new()
        .name("tandem-span".into())
        .spawn(move || {
            // End Windows' move: Esc puts the window back where the drag started and
            // drops the snap preview; the button-up keeps Windows' idea of the button
            // right (the real one will be swallowed). Then put the window back here.
            send(&[
                key_input(VK_ESCAPE.0, 0, false, true),
                key_input(VK_ESCAPE.0, 0, false, false),
            ]);
            std::thread::sleep(Duration::from_millis(45));
            send(&[mouse_input(0, 0, 0, MOUSEEVENTF_LEFTUP)]);
            std::thread::sleep(Duration::from_millis(15));
            if let Ok(mut g) = OUT.lock() {
                if let Some(o) = g.as_mut() {
                    o.settled = true;
                    place_window(o);
                }
            }
        });
    true
}

fn place_window(o: &Out) {
    let x = o.vx - o.geo.gx - o.off_x;
    let y = o.vy - o.geo.gy - o.off_y;
    unsafe {
        let _ = SetWindowPos(
            hwnd(o.hwnd),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

fn report(o: &mut Out, force: bool) {
    if !force && o.last_sent.elapsed() < Duration::from_millis(14) {
        return;
    }
    o.last_sent = Instant::now();
    emit(CaptureEvent::SpanMove {
        crossed: o.geo.crossed(o.vx, o.vy),
        along: o.geo.along(o.vx, o.vy),
        gx: o.geo.gx,
        gy: o.geo.gy,
    });
}

fn on_screen(s: Screen, x: i32, y: i32) -> bool {
    x >= s.x && y >= s.y && x < s.x + s.width && y < s.y + s.height
}

/// The mouse moved by (`dx`, `dy`) while this computer moves the window.
pub fn mouse(dx: i32, dy: i32) {
    let Ok(mut g) = OUT.try_lock() else {
        return;
    };
    let Some(o) = g.as_mut() else {
        return;
    };
    if o.phase != Phase::Driven {
        return;
    }
    let (vx, vy) = o.geo.clamp(o.vx + dx, o.vy + dy);
    if (vx, vy) == (o.vx, o.vy) {
        return;
    }
    o.vx = vx;
    o.vy = vy;
    if o.settled {
        place_window(o);
    }
    let s = o.geo.scr;
    cursor_overlay(on_screen(s, vx, vy).then_some((vx, vy)));
    report(o, false);
}

/// The left button went up. During a preview Windows drops the window where it is, so
/// it is nudged fully back on screen. While driven: let go more than halfway over and
/// the program moves (the engine is told, with the program, in `SpanEnd`); otherwise the
/// window comes back and the pointer is given back here. Returns (moved, edge, where
/// along the edge) for a driven drag: when moved by this computer's own mouse, input
/// stays swallowed for the `Crossed` the caller sends next.
pub fn release() -> Option<(bool, Edge, f32)> {
    let mut o = OUT.lock().ok()?.take()?;
    let driven = o.phase == Phase::Driven;
    finish(&o);
    cursor_overlay(None);
    if !driven {
        emit(CaptureEvent::SpanEnd {
            moved: false,
            app: None,
            frac: 0.0,
        });
        let (h, s) = (o.hwnd, o.geo.scr);
        let _ = std::thread::Builder::new().spawn(move || {
            // Windows finishes its move first.
            std::thread::sleep(Duration::from_millis(60));
            restore_window(h, s);
        });
        return None;
    }
    let moved = o.geo.moved(o.vx, o.vy);
    report(&mut o, true);
    let frac = o.geo.frac(o.vx, o.vy);
    log::info!(
        "app span: let go with {} px over: {}",
        o.geo.crossed(o.vx, o.vy),
        if moved {
            "moving the program"
        } else {
            "staying here"
        }
    );
    if moved {
        if o.remote {
            hide_pointer(false);
        }
        let app = span_app(o.hwnd);
        if app.is_none() {
            log::info!("app span: that window's program can't be moved");
        }
        emit(CaptureEvent::SpanEnd {
            moved: true,
            app,
            frac,
        });
    } else {
        give_back(&mut o);
        emit(CaptureEvent::SpanEnd {
            moved: false,
            app: None,
            frac,
        });
    }
    Some((moved, o.geo.edge, frac))
}

/// Esc, a right click or the emergency key during the drag: keep the window here.
/// True if one was on.
pub fn cancel() -> bool {
    let Some(mut o) = OUT.lock().ok().and_then(|mut g| g.take()) else {
        return false;
    };
    let driven = o.phase == Phase::Driven;
    finish(&o);
    cursor_overlay(None);
    if driven {
        give_back(&mut o);
    }
    emit(CaptureEvent::SpanEnd {
        moved: false,
        app: None,
        frac: 0.0,
    });
    true
}

fn give_back(o: &mut Out) {
    let (bx, by) = o.geo.back_inside(o.vx, o.vy);
    o.vx = bx;
    o.vy = by;
    place_window(o);
    if o.remote {
        hide_pointer(false);
    } else {
        set_capturing(false);
    }
    let s = o.geo.scr;
    let x = bx.clamp(s.x, s.x + s.width - 1);
    let y = by.clamp(s.y, s.y + s.height - 1);
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// The program didn't open on the other computer: bring its window fully back here.
pub fn restore() {
    if let Some((h, s)) = LAST.lock().ok().and_then(|mut l| l.take()) {
        restore_window(h, s);
    }
}

fn restore_window(h: isize, s: Screen) {
    let (Some(full), Some(vis)) = (window_rect(h), visible_rect(h)) else {
        return;
    };
    let (w, ht) = (vis.right - vis.left, vis.bottom - vis.top);
    let nx = vis.left.clamp(s.x, (s.x + s.width - w).max(s.x));
    let ny = vis.top.clamp(s.y, (s.y + s.height - ht).max(s.y));
    if (nx, ny) == (vis.left, vis.top) {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd(h),
            None,
            full.left + nx - vis.left,
            full.top + ny - vis.top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// Is a preview or drive on (for the hooks, without locking)?
pub fn showing() -> bool {
    SHOWING.load(Relaxed)
}

/// Pictures of the window while it hangs across.
fn pictures(gen: u32, h: isize, off: (i32, i32), w: i32, ht: i32) {
    let mut sent = 0u32;
    while GEN.load(Relaxed) == gen {
        let t = Instant::now();
        if let Some(jpg) = grab(h, off, w, ht) {
            sent += 1;
            emit(CaptureEvent::SpanFrame(jpg));
        }
        let left = Duration::from_millis(span::FRAME_EVERY_MS).saturating_sub(t.elapsed());
        std::thread::sleep(left.max(Duration::from_millis(15)));
    }
    log::info!("app span: {sent} pictures sent");
}

/// One picture of the window's visible part, as JPEG.
fn grab(h: isize, (ox, oy): (i32, i32), w: i32, ht: i32) -> Option<Vec<u8>> {
    let r = window_rect(h)?;
    let (fw, fh) = (r.right - r.left, r.bottom - r.top);
    if fw <= 0 || fh <= 0 || fw > 8192 || fh > 8192 {
        return None;
    }
    let px = unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let (bmp, bits) = dib(screen, fw, fh)?;
        let old = SelectObject(mem, bmp.into());
        // 2 = PW_RENDERFULLCONTENT: also windows drawn with DirectX (browsers, Electron).
        let ok = PrintWindow(hwnd(h), mem, PRINT_WINDOW_FLAGS(2)).as_bool();
        let px = ok.then(|| std::slice::from_raw_parts(bits, (fw * fh * 4) as usize).to_vec());
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        px?
    };
    // Cut out the visible part.
    let (cw, ch) = (w.min(fw - ox).max(1), ht.min(fh - oy).max(1));
    let mut vis = Vec::with_capacity((cw * ch * 4) as usize);
    for y in 0..ch {
        let row = (((oy + y) * fw + ox) * 4) as usize;
        vis.extend_from_slice(px.get(row..row + (cw * 4) as usize)?);
    }
    let (sw, sh) = span::fit(cw as u32, ch as u32, span::FRAME_MAX_W, span::FRAME_MAX_H);
    let small = if (sw, sh) == (cw as u32, ch as u32) {
        vis
    } else {
        span::scale4(&vis, cw as u32, ch as u32, sw, sh)
    };
    span::encode_jpeg(&small, sw, sh, span::FRAME_QUALITY)
}

/// A top-down 32-bit bitmap and its pixels.
unsafe fn dib(dc: HDC, w: i32, h: i32) -> Option<(HBITMAP, *mut u8)> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bmp.into());
        return None;
    }
    Some((bmp, bits as *mut u8))
}

// ------------------------------------------------------------------ the windows on screen
//
// One thread owns two layered windows: the picture of a window coming over from another
// computer, and the stand-in arrow while this computer drags a window across. Anything
// that wants them changed queues a command and pokes the thread.

enum Cmd {
    Show(SpanView),
    Frame(Vec<u8>),
    Hide(u32),
    Cursor(Option<(i32, i32)>),
}

static QUEUE: Mutex<VecDeque<Cmd>> = Mutex::new(VecDeque::new());
static PIC_HWND: AtomicIsize = AtomicIsize::new(0);
static ARROW_HWND: AtomicIsize = AtomicIsize::new(0);
static START: Once = Once::new();
const WM_SPAN: u32 = WM_APP + 0x51;
const HIDE_TIMER: usize = 1;

fn post(c: Cmd) {
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-span-ui".into())
            .spawn(ui_thread);
    });
    if let Ok(mut q) = QUEUE.lock() {
        // A newer position or picture makes an older one pointless.
        let same = |a: &Cmd, b: &Cmd| {
            matches!(
                (a, b),
                (Cmd::Show(_), Cmd::Show(_))
                    | (Cmd::Frame(_), Cmd::Frame(_))
                    | (Cmd::Cursor(_), Cmd::Cursor(_))
            )
        };
        q.retain(|old| !same(old, &c));
        q.push_back(c);
    }
    let h = PIC_HWND.load(Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(hwnd(h)), WM_SPAN, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn show(v: SpanView) {
    post(Cmd::Show(v));
}
pub fn frame(jpeg: Vec<u8>) {
    post(Cmd::Frame(jpeg));
}
pub fn hide(after_ms: u32) {
    post(Cmd::Hide(after_ms));
}
fn cursor_overlay(at: Option<(i32, i32)>) {
    post(Cmd::Cursor(at));
}

#[derive(Default)]
struct Ui {
    view: Option<SpanView>,
    frame: Option<(Vec<u8>, u32, u32)>,
    dirty: bool,
    shown: bool,
    hide_at: Option<Instant>,
    arrow_shown: bool,
}

static UI: Mutex<Option<Ui>> = Mutex::new(None);

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SPAN | WM_TIMER => {
            let _ = std::panic::catch_unwind(work);
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(-1), // HTTRANSPARENT: clicks go to whatever is below
        WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

fn ui_thread() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let hinst = HINSTANCE(module.0);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: w!("TandemSpan"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let ex =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let make = || {
            CreateWindowExW(
                ex,
                w!("TandemSpan"),
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinst),
                None,
            )
            .map(|h| h.0 as isize)
            .unwrap_or(0)
        };
        let (pic, arrow) = (make(), make());
        if pic == 0 || arrow == 0 {
            log::warn!("app span: cannot create the picture window");
            return;
        }
        ARROW_HWND.store(arrow, Relaxed);
        if let Ok(mut u) = UI.lock() {
            *u = Some(Ui::default());
        }
        // The arrow never changes: draw it once.
        let (px, aw, ah, _, _) = span::arrow();
        layered(arrow, -100, -100, aw as i32, ah as i32, &px);
        let _ = ShowWindow(hwnd(arrow), SW_HIDE);
        PIC_HWND.store(pic, Relaxed);
        work(); // anything queued while starting
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Puts premultiplied BGRA pixels on a layered window at (x, y).
fn layered(h: isize, x: i32, y: i32, w: i32, ht: i32, px: &[u8]) {
    if w <= 0 || ht <= 0 || px.len() < (w * ht * 4) as usize {
        return;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        if let Some((bmp, bits)) = dib(screen, w, ht) {
            std::ptr::copy_nonoverlapping(px.as_ptr(), bits, (w * ht * 4) as usize);
            let old = SelectObject(mem, bmp.into());
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd(h),
                Some(screen),
                Some(&POINT { x, y }),
                Some(&SIZE { cx: w, cy: ht }),
                Some(mem),
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

fn top_at(h: isize, x: i32, y: i32) {
    unsafe {
        let _ = SetWindowPos(
            hwnd(h),
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Runs on the UI thread: applies what was queued.
fn work() {
    let pic = PIC_HWND.load(Relaxed);
    let arrow = ARROW_HWND.load(Relaxed);
    let Ok(mut guard) = UI.lock() else {
        return;
    };
    let Some(ui) = guard.as_mut() else {
        return;
    };
    let cmds: Vec<Cmd> = QUEUE
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    let mut hide_now = false;
    for c in cmds {
        match c {
            Cmd::Show(v) => {
                if ui.view.map(|o| (o.w, o.h, o.gx, o.gy)) != Some((v.w, v.h, v.gx, v.gy)) {
                    ui.dirty = true;
                }
                ui.view = Some(v);
                ui.hide_at = None;
            }
            Cmd::Frame(j) => {
                if let Some(f) = span::decode_jpeg(&j) {
                    ui.frame = Some(f);
                    ui.dirty = true;
                }
            }
            Cmd::Hide(0) => hide_now = true,
            Cmd::Hide(ms) => {
                ui.hide_at = Some(Instant::now() + Duration::from_millis(ms as u64));
                unsafe {
                    SetTimer(Some(hwnd(pic)), HIDE_TIMER, 100, None);
                }
            }
            Cmd::Cursor(Some((x, y))) => {
                let (tx, ty) = (3, 2); // where the arrow's tip is in its picture
                top_at(arrow, x - tx, y - ty);
                ui.arrow_shown = true;
            }
            Cmd::Cursor(None) => {
                if ui.arrow_shown {
                    unsafe {
                        let _ = ShowWindow(hwnd(arrow), SW_HIDE);
                    }
                    ui.arrow_shown = false;
                }
            }
        }
    }
    if ui.hide_at.is_some_and(|t| Instant::now() >= t) {
        hide_now = true;
    }
    if hide_now {
        unsafe {
            let _ = KillTimer(Some(hwnd(pic)), HIDE_TIMER);
            let _ = ShowWindow(hwnd(pic), SW_HIDE);
        }
        ui.view = None;
        ui.frame = None;
        ui.hide_at = None;
        ui.shown = false;
        ui.dirty = false;
        return;
    }
    let Some(v) = ui.view else {
        return;
    };
    let (x, y) = span::place(&v, vscreen());
    if ui.dirty || !ui.shown {
        let f = ui.frame.as_ref().map(|(p, w, h)| (p.as_slice(), *w, *h));
        let px = span::compose(f, v.w, v.h, v.gx, v.gy);
        layered(pic, x, y, v.w as i32, v.h as i32, &px);
        ui.dirty = false;
    }
    top_at(pic, x, y);
    ui.shown = true;
}
