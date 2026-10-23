//! Mac side of dragging a window across the edge (see `span.rs`, and `winspan.rs` for
//! the same on Windows).
//!
//! macOS doesn't say which window is being dragged, so the window under the pointer is
//! noted when the button goes down, and "being moved" means: it has moved since, and
//! kept its size.
//!
//! * Preview: macOS still moves the window. As soon as part of it is past an edge that
//!   leads to another computer, that computer shows the part that is over.
//! * Driven: the pointer reaches the edge. macOS' own drag is ended (a button-up of
//!   ours) and this computer moves the window itself, through Accessibility, as far over
//!   as the mouse pushes it. The real pointer is held still and hidden; a stand-in
//!   arrow follows.

use crate::macwin::{self, Ax, WindowId};
use crate::platform::{CaptureEvent, Screen};
use crate::proto::{AppRef, Edge};
use crate::span::{self, Geo};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Condvar, Mutex, Once};
use std::time::{Duration, Instant};

fn emit(ev: CaptureEvent) {
    crate::mac::emit_pub(ev);
}

fn now_us() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_micros() as u64
}

// ------------------------------------------------------------------ which window is held

#[derive(Clone)]
struct Press {
    id: WindowId,
    pid: i32,
    start: macwin::R,
    /// For a browser: the page in it (read in the background).
    url: Option<String>,
}

static PRESS: Mutex<Option<Press>> = Mutex::new(None);
static PRESS_GEN: AtomicU64 = AtomicU64::new(0);

/// The left button went down at (`x`, `y`) (this computer's own mouse, or the other
/// computer's): note the window there.
pub fn pressed(x: f64, y: f64) {
    let gen = PRESS_GEN.fetch_add(1, Relaxed) + 1;
    // Asking the window server takes a moment: not on the event tap's thread.
    std::thread::spawn(move || {
        let found = macwin::at(x, y).map(|w| Press {
            id: w.id,
            pid: w.pid,
            start: w.bounds,
            url: None,
        });
        if PRESS_GEN.load(Relaxed) != gen {
            return;
        }
        let browser = found
            .as_ref()
            .and_then(|p| crate::macapps::app_of_pid(p.pid))
            .filter(|a| a.is_browser());
        if let Ok(mut g) = PRESS.lock() {
            *g = found;
        }
        // A browser window: its page, in case it goes to the other computer.
        if let Some(app) = browser {
            if let Some(url) = crate::macapps::browser_url(&app) {
                if PRESS_GEN.load(Relaxed) == gen {
                    if let Ok(mut g) = PRESS.lock() {
                        if let Some(p) = g.as_mut() {
                            p.url = Some(url);
                        }
                    }
                }
            }
        }
    });
}

/// The button went up: nothing is held any more.
pub fn released() {
    PRESS_GEN.fetch_add(1, Relaxed);
    if let Ok(mut g) = PRESS.lock() {
        *g = None;
    }
}

/// The window being moved by the person right now, if any: the one under the press,
/// moved since, same size.
fn moving_window() -> Option<Press> {
    let p = PRESS.lock().ok()?.clone()?;
    let now = macwin::info(p.id)?.bounds;
    let moved = (now.x - p.start.x).abs() >= 1.0 || (now.y - p.start.y).abs() >= 1.0;
    let same = (now.w - p.start.w).abs() < 1.0 && (now.h - p.start.h).abs() < 1.0;
    (moved && same).then_some(Press { start: now, ..p })
}

/// The program of window-owner `pid` as it would travel, with the page for a browser.
fn app_for(p: &Press) -> Option<AppRef> {
    let mut app = crate::macapps::app_of_pid(p.pid)?;
    if app.is_browser() {
        app.url = p.url.clone();
    }
    app.is_sane().then_some(app)
}

// ------------------------------------------------------------------ sending

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Preview,
    Driven,
}

struct Out {
    geo: Geo,
    press: Press,
    ax: Option<std::sync::Arc<Ax>>,
    vx: i32,
    vy: i32,
    phase: Phase,
    remote: bool,
    last_sent: Instant,
}

static OUT: Mutex<Option<Out>> = Mutex::new(None);
/// The driven phase is on: the tap routes the mouse here.
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
static SHOWING: AtomicBool = AtomicBool::new(false);
static GEN: AtomicU32 = AtomicU32::new(0);
static LAST_WATCH_US: AtomicU64 = AtomicU64::new(0);
/// The last window that hung across (id, owner), to keep it here or put it back.
static LAST: Mutex<Option<(WindowId, i32)>> = Mutex::new(None);

fn start(
    p: Press,
    edges: &[Edge],
    pt: (i32, i32),
    scr: Screen,
    remote: bool,
    at_edge: Option<Edge>,
) -> Option<Out> {
    let b = p.start;
    let (w, h) = (b.w.round() as i32, b.h.round() as i32);
    if w < 40 || h < 30 {
        return None;
    }
    let (left, top) = (b.x.round() as i32, b.y.round() as i32);
    let geo_for = |edge: Edge| Geo {
        edge,
        scr,
        w,
        h,
        gx: (pt.0 - left).clamp(0, w - 1),
        gy: (pt.1 - top).clamp(0, h - 1),
    };
    let edge = match at_edge {
        Some(e) => e,
        None => *edges
            .iter()
            .find(|e| !e.is_corner() && geo_for(**e).crossed(pt.0, pt.1) > 0)?,
    };
    Some(Out {
        geo: geo_for(edge),
        press: p,
        ax: None,
        vx: pt.0,
        vy: pt.1,
        phase: Phase::Preview,
        remote,
        last_sent: Instant::now() - Duration::from_secs(1),
    })
}

fn announce(o: &mut Out) {
    let gen = GEN.fetch_add(1, Relaxed) + 1;
    SHOWING.store(true, Relaxed);
    log::info!(
        "app span: {}x{} window crosses the {:?} edge",
        o.geo.w,
        o.geo.h,
        o.geo.edge
    );
    emit(CaptureEvent::SpanBegin {
        edge: o.geo.edge,
        w: o.geo.w as u32,
        h: o.geo.h as u32,
    });
    report(o, true);
    let id = o.press.id;
    let _ = std::thread::Builder::new()
        .name("tandem-span-pics".into())
        .spawn(move || pictures(gen, id));
}

fn finish(o: &Out) {
    GEN.fetch_add(1, Relaxed);
    SHOWING.store(false, Relaxed);
    ACTIVE.store(false, Relaxed);
    if let Ok(mut l) = LAST.lock() {
        *l = Some((o.press.id, o.press.pid));
    }
}

fn end_preview_event() {
    emit(CaptureEvent::SpanEnd {
        moved: false,
        app: None,
        frac: 0.0,
    });
}

/// While a window is being moved: show it on the other computer as soon as part of it
/// is past one of `edges`, and stop as soon as it is all back.
pub fn watch(edges: &[Edge], pt: (i32, i32), scr: Screen, remote: bool) {
    if ACTIVE.load(Relaxed) {
        return;
    }
    let now = now_us();
    if now.saturating_sub(LAST_WATCH_US.load(Relaxed)) < 16_000 {
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
        let still = moving.as_ref().is_some_and(|m| m.id == o.press.id) && {
            o.vx = pt.0;
            o.vy = pt.1;
            o.geo.crossed(pt.0, pt.1) > 0
        };
        if still {
            report(o, false);
            return;
        }
        let o = g.take().unwrap();
        finish(&o);
        end_preview_event();
    }
    let Some(p) = moving else {
        return;
    };
    if let Some(mut o) = start(p, edges, pt, scr, remote, None) {
        announce(&mut o);
        *g = Some(o);
    }
}

/// The pointer reached `edge` while a window is held: from here this computer moves it.
pub fn take(edge: Edge, pt: (i32, i32), scr: Screen, remote: bool) -> bool {
    if edge.is_corner() || ACTIVE.load(Relaxed) {
        return false;
    }
    let Ok(mut g) = OUT.lock() else {
        return false;
    };
    let moving = moving_window();
    let fresh = match g.as_ref() {
        Some(o) => o.geo.edge != edge || moving.as_ref().map(|m| m.id) != Some(o.press.id),
        None => true,
    };
    if fresh {
        if let Some(o) = g.take() {
            finish(&o);
            end_preview_event();
        }
        let Some(p) = moving else {
            return false;
        };
        let Some(mut o) = start(p, &[edge], pt, scr, remote, Some(edge)) else {
            return false;
        };
        announce(&mut o);
        *g = Some(o);
    }
    let o = g.as_mut().unwrap();
    o.phase = Phase::Driven;
    o.vx = pt.0;
    o.vy = pt.1;
    o.ax = macwin::ax_window(o.press.pid, o.press.id).map(std::sync::Arc::new);
    ACTIVE.store(true, Relaxed);
    // End macOS' own drag (a button-up of ours), and hold the pointer still.
    crate::mac::post_left_up_pub();
    crate::mac::hold_pointer_pub(true);
    crate::macoverlay::cursor_overlay(Some(pt));
    log::info!("app span: the pointer reached the edge: CubeCursor moves the window now");
    place_window(o);
    true
}

// The window is moved on a thread of its own: Accessibility waits for the program.
static MOVE_TO: Mutex<Option<(std::sync::Arc<Ax>, f64, f64)>> = Mutex::new(None);
static MOVE_WAKE: Condvar = Condvar::new();
static MOVER: Once = Once::new();

fn mover() {
    loop {
        let job = {
            let Ok(mut g) = MOVE_TO.lock() else {
                return;
            };
            while g.is_none() {
                g = match MOVE_WAKE.wait(g) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
            g.take()
        };
        if let Some((ax, x, y)) = job {
            ax.move_to(x, y);
        }
    }
}

pub(crate) fn move_window(ax: std::sync::Arc<Ax>, x: f64, y: f64) {
    MOVER.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-mover".into())
            .spawn(mover);
    });
    if let Ok(mut g) = MOVE_TO.lock() {
        *g = Some((ax, x, y));
        MOVE_WAKE.notify_one();
    }
}

fn place_window(o: &Out) {
    if let Some(ax) = &o.ax {
        let x = (o.vx - o.geo.gx) as f64;
        let y = (o.vy - o.geo.gy) as f64;
        move_window(ax.clone(), x, y);
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
    place_window(o);
    let s = o.geo.scr;
    crate::macoverlay::cursor_overlay(on_screen(s, vx, vy).then_some((vx, vy)));
    report(o, false);
}

/// The left button went up (see `winspan::release`).
pub fn release() -> Option<(bool, Edge, f32)> {
    let mut o = OUT.lock().ok()?.take()?;
    let driven = o.phase == Phase::Driven;
    finish(&o);
    crate::macoverlay::cursor_overlay(None);
    if !driven {
        end_preview_event();
        let (id, pid, s) = (o.press.id, o.press.pid, o.geo.scr);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            restore_window(id, pid, s);
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
            crate::mac::hold_pointer_pub(false);
        }
        let app = app_for(&o.press);
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

/// Esc or a right click during the drag: keep the window here.
pub fn cancel() -> bool {
    let Some(mut o) = OUT.lock().ok().and_then(|mut g| g.take()) else {
        return false;
    };
    let driven = o.phase == Phase::Driven;
    finish(&o);
    crate::macoverlay::cursor_overlay(None);
    if driven {
        give_back(&mut o);
    }
    end_preview_event();
    true
}

fn give_back(o: &mut Out) {
    let (bx, by) = o.geo.back_inside(o.vx, o.vy);
    o.vx = bx;
    o.vy = by;
    place_window(o);
    crate::mac::hold_pointer_pub(false);
    let s = o.geo.scr;
    let x = bx.clamp(s.x, s.x + s.width - 1);
    let y = by.clamp(s.y, s.y + s.height - 1);
    crate::mac::warp_pub(x as f64, y as f64);
}

/// The program didn't open on the other computer: bring its window back here.
pub fn restore() {
    if let Some((id, pid)) = LAST.lock().ok().and_then(|mut l| l.take()) {
        restore_window(id, pid, crate::mac::desktop_pub());
    }
}

/// The last window that hung across, to keep it here (second-monitor mode).
pub fn take_last() -> Option<(WindowId, i32)> {
    LAST.lock().ok().and_then(|mut l| l.take())
}

fn restore_window(id: WindowId, pid: i32, s: Screen) {
    let Some(w) = macwin::info(id) else {
        return;
    };
    let b = w.bounds;
    let nx =
        b.x.clamp(s.x as f64, ((s.x + s.width) as f64 - b.w).max(s.x as f64));
    let ny =
        b.y.clamp(s.y as f64, ((s.y + s.height) as f64 - b.h).max(s.y as f64));
    if (nx - b.x).abs() < 1.0 && (ny - b.y).abs() < 1.0 {
        return;
    }
    if let Some(ax) = macwin::ax_window(pid, id) {
        ax.move_to(nx, ny);
    }
}

pub fn showing() -> bool {
    SHOWING.load(Relaxed)
}

/// Pictures of the window while it hangs across.
fn pictures(gen: u32, id: WindowId) {
    let mut sent = 0u32;
    while GEN.load(Relaxed) == gen {
        let t = Instant::now();
        if let Some((px, w, h)) = macwin::picture(id) {
            let (sw, sh) = span::fit(w, h, span::FRAME_MAX_W, span::FRAME_MAX_H);
            let small = if (sw, sh) == (w, h) {
                px
            } else {
                span::scale4(&px, w, h, sw, sh)
            };
            if let Some(jpg) = span::encode_jpeg(&small, sw, sh, span::FRAME_QUALITY) {
                sent += 1;
                emit(CaptureEvent::SpanFrame(jpg));
            }
        }
        let left = Duration::from_millis(span::FRAME_EVERY_MS).saturating_sub(t.elapsed());
        std::thread::sleep(left.max(Duration::from_millis(15)));
    }
    log::info!("app span: {sent} pictures sent");
}
