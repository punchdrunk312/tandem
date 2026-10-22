//! Windows side of dragging a window across the edge (see `span.rs`).
//!
//! Sending: once a window held by its title bar is pushed through a connected edge, this
//! computer stops Windows' own move (Esc, which also dismisses the snap preview), hides
//! the pointer like it does when driving another computer, and moves the window itself
//! with the mouse, letting it hang off the screen. A small arrow stands in for the pointer
//! while it is over this screen. A thread takes pictures of the window (`PrintWindow`
//! also draws the part that is off-screen) and hands them to the engine as JPEG.
//!
//! Receiving: a click-through, never-focused, always-on-top layered window shows the
//! picture where the window would be on this screen.
//!
//! Everything the mouse hook calls only touches atomics, a mutex nobody holds for long,
//! and non-blocking window calls; the waiting happens on threads of its own.

use crate::platform::{CaptureEvent, Screen, SpanView};
use crate::proto::Edge;
use crate::span::{self, Geo};
use crate::win::{emit, key_input, mouse_input, send, set_capturing, vscreen};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::Relaxed};
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
    /// Windows' own move has been stopped: from now on this computer moves the window.
    settled: bool,
    last_sent: Instant,
}

static OUT: Mutex<Option<Out>> = Mutex::new(None);
/// A window hangs across an edge right now (read by the hooks without locking).
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Bumped for every drag, so a picture thread from an earlier one stops.
static GEN: AtomicU32 = AtomicU32::new(0);
/// The last window that hung across, to put it back if the program didn't open there.
static LAST: Mutex<Option<(isize, Screen)>> = Mutex::new(None);

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
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

/// The window held at `pt` was pushed through `edge`. Starts the drag across and returns
/// the window's size, or None (then the usual "hold to move" offer happens instead).
pub fn begin(h: isize, edge: Edge, pt: (i32, i32), scr: Screen) -> Option<(u32, u32)> {
    if h == 0 || edge.is_corner() || ACTIVE.load(Relaxed) {
        return None;
    }
    let full = window_rect(h)?;
    let vis = visible_rect(h)?;
    let (w, ht) = (vis.right - vis.left, vis.bottom - vis.top);
    if w < 40 || ht < 30 {
        return None;
    }
    let geo = Geo {
        edge,
        scr,
        w,
        h: ht,
        gx: (pt.0 - vis.left).clamp(0, w - 1),
        gy: (pt.1 - vis.top).clamp(0, ht - 1),
    };
    let out = Out {
        geo,
        hwnd: h,
        off_x: vis.left - full.left,
        off_y: vis.top - full.top,
        vx: pt.0,
        vy: pt.1,
        settled: false,
        last_sent: Instant::now(),
    };
    let off = (out.off_x, out.off_y);
    *OUT.lock().ok()? = Some(out);
    let gen = GEN.fetch_add(1, Relaxed) + 1;
    ACTIVE.store(true, Relaxed);
    // Swallow the mouse from now on and hide the real pointer (parked mid-screen).
    set_capturing(true);
    let (cx, cy) = scr.center();
    unsafe {
        let _ = SetCursorPos(cx, cy);
    }
    cursor_overlay(Some(pt));
    log::info!("app span: {w}x{ht} window pushed through the {edge:?} edge");
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
            pictures(gen, h, off, w, ht);
        });
    Some((w as u32, ht as u32))
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

/// The mouse moved by (`dx`, `dy`) while the window hangs across.
pub fn mouse(dx: i32, dy: i32) {
    let Ok(mut g) = OUT.try_lock() else {
        return;
    };
    let Some(o) = g.as_mut() else {
        return;
    };
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

/// The button went up. Returns (moved over, edge, where along the edge) when a drag was
/// on. Not moved: the window is back on this screen and the pointer is given back here.
/// Moved: input stays swallowed, for the `Crossed` the caller sends next.
pub fn release() -> Option<(bool, Edge, f32)> {
    let mut o = OUT.lock().ok()?.take()?;
    ACTIVE.store(false, Relaxed);
    cursor_overlay(None);
    let moved = o.geo.moved(o.vx, o.vy);
    report(&mut o, true);
    if let Ok(mut l) = LAST.lock() {
        *l = Some((o.hwnd, o.geo.scr));
    }
    log::info!(
        "app span: let go with {} of {} px over: {}",
        o.geo.crossed(o.vx, o.vy),
        if matches!(o.geo.edge, Edge::Left | Edge::Right) {
            o.geo.w
        } else {
            o.geo.h
        },
        if moved {
            "moving the program"
        } else {
            "staying here"
        }
    );
    if !moved {
        give_back(&mut o);
    }
    Some((moved, o.geo.edge, o.geo.frac(o.vx, o.vy)))
}

/// Esc (or the emergency key) during the drag: keep the window here. True if one was on.
pub fn cancel() -> bool {
    let Some(mut o) = OUT.lock().ok().and_then(|mut g| g.take()) else {
        return false;
    };
    ACTIVE.store(false, Relaxed);
    cursor_overlay(None);
    give_back(&mut o);
    true
}

fn give_back(o: &mut Out) {
    let (bx, by) = o.geo.back_inside(o.vx, o.vy);
    o.vx = bx;
    o.vy = by;
    place_window(o);
    set_capturing(false);
    let s = o.geo.scr;
    let x = bx.clamp(s.x, s.x + s.width - 1);
    let y = by.clamp(s.y, s.y + s.height - 1);
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// The program didn't open on the other computer: bring its window fully back here.
pub fn restore() {
    let Some((h, s)) = LAST.lock().ok().and_then(|mut l| l.take()) else {
        return;
    };
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

/// Pictures of the window while it hangs across.
fn pictures(gen: u32, h: isize, off: (i32, i32), w: i32, ht: i32) {
    let mut sent = 0u32;
    while GEN.load(Relaxed) == gen && ACTIVE.load(Relaxed) {
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
