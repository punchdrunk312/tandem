//! Mac side of second-monitor mode (see `mirror.rs`, and `winmirror.rs` for Windows).
//!
//! The owner keeps the real window, parked just past the edge, and slides it onto its own
//! screen while the pointer is on the mirror. macOS has no "always on top" for other
//! programs' windows, so the program is brought to the front then (through
//! Accessibility), so clicks land on it.
//!
//! The mirror end shows each one in an ordinary window of ours.

use crate::macwin::{self, Ax, WindowId};
use crate::mirror::{self, Rect};
use crate::platform::CaptureEvent;
use crate::proto::Edge;
use crate::span;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

fn emit(ev: CaptureEvent) {
    crate::mac::emit_pub(ev);
}

fn cursor() -> (i32, i32) {
    crate::mac::cursor_pub()
}

fn r_of(b: macwin::R) -> Rect {
    Rect {
        x: b.x.round() as i32,
        y: b.y.round() as i32,
        w: b.w.round() as i32,
        h: b.h.round() as i32,
    }
}

// ================================================================== the owner

struct Owned {
    id: WindowId,
    pid: i32,
    ax: Arc<Ax>,
    edge: Edge,
    parked: (i32, i32),
    work: Rect,
    slid: Option<Rect>,
    seen: (i32, i32),
    dragging: bool,
    fresh: bool,
}

static OWNED: Mutex<Vec<Owned>> = Mutex::new(Vec::new());
static PORTAL_ID: AtomicU64 = AtomicU64::new(0);
static PORTAL_ON: AtomicBool = AtomicBool::new(false);
static PR: [AtomicI32; 4] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];
static THREADS: Once = Once::new();

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

/// The display a window is on, without the menu bar.
fn work_area(b: macwin::R) -> Rect {
    use core_graphics::display::CGDisplay;
    let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    let ids = CGDisplay::active_displays().unwrap_or_default();
    let d = ids
        .into_iter()
        .map(|id| CGDisplay::new(id).bounds())
        .find(|r| {
            cx >= r.origin.x
                && cy >= r.origin.y
                && cx < r.origin.x + r.size.width
                && cy < r.origin.y + r.size.height
        })
        .unwrap_or_else(|| CGDisplay::main().bounds());
    let menu = 28.0;
    Rect {
        x: d.origin.x as i32,
        y: (d.origin.y + menu) as i32,
        w: d.size.width as i32,
        h: (d.size.height - menu) as i32,
    }
}

/// The pointer is done with a window slid in: the veil goes, the pointer shows again
/// (unless the mouse is driving the other computer).
fn portal_over() {
    crate::macoverlay::veil(None);
    crate::mac::show_pointer_unless_captured();
}

fn park(o: &mut Owned, size: (i32, i32)) {
    let (x, y) = mirror::park_at(crate::mac::desktop_pub(), o.edge, size.0, size.1, o.parked);
    o.ax.move_to(x as f64, y as f64);
    o.parked = (x, y);
    o.seen = (x, y);
    o.slid = None;
}

pub fn adopt(edge: Edge) -> Option<(u64, u32, u32)> {
    let (id, pid) = crate::macspan::take_last()?;
    let info = macwin::info(id)?;
    let ax = Arc::new(macwin::ax_window(pid, id)?);
    if ax.flag("AXFullScreen") {
        return None;
    }
    let mut b = r_of(info.bounds);
    let work = work_area(info.bounds);
    let fit = mirror::slide_in(work, b.w, b.h);
    if (fit.w, fit.h) != (b.w, b.h) {
        ax.resize(fit.w as f64, fit.h as f64);
        b.w = fit.w;
        b.h = fit.h;
    }
    let mut o = Owned {
        id,
        pid,
        ax,
        edge,
        parked: (b.x, b.y),
        work,
        slid: None,
        seen: (b.x, b.y),
        dragging: false,
        fresh: true,
    };
    park(&mut o, (b.w, b.h));
    if let Ok(mut all) = OWNED.lock() {
        all.retain(|x| x.id != id);
        all.push(o);
    }
    THREADS.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-mirror-pics".into())
            .spawn(pictures);
        let _ = std::thread::Builder::new()
            .name("tandem-mirror-cursor".into())
            .spawn(cursor_watch);
    });
    log::info!(
        "mirror: keeping window {id} here ({}x{}), parked past the {edge:?} edge",
        b.w,
        b.h
    );
    Some((id as u64, b.w as u32, b.h as u32))
}

pub fn release(id: u64, home: Option<f32>) {
    let Some(o) = OWNED.lock().ok().and_then(|mut all| {
        let i = all.iter().position(|o| o.id as u64 == id)?;
        Some(all.remove(i))
    }) else {
        return;
    };
    let was_portal = PORTAL_ON.load(Relaxed) && PORTAL_ID.load(Relaxed) == id;
    if was_portal {
        PORTAL_ON.store(false, Relaxed);
        portal_over();
    }
    let Some(b) = macwin::info(o.id).map(|w| r_of(w.bounds)) else {
        return;
    };
    let (x, y) = match home {
        Some(along) => mirror::home_at(crate::mac::desktop_pub(), o.edge, along, b.w, b.h),
        None => {
            let r = mirror::slide_in(o.work, b.w, b.h);
            (r.x, r.y)
        }
    };
    o.ax.move_to(x as f64, y as f64);
    if was_portal {
        let (cx, cy) = cursor();
        crate::mac::warp_pub((cx + x - b.x) as f64, (cy + y - b.y) as f64);
    }
}

pub fn release_all() {
    let ids: Vec<u64> = OWNED
        .lock()
        .map(|all| all.iter().map(|o| o.id as u64).collect())
        .unwrap_or_default();
    for id in ids {
        release(id, None);
    }
}

pub fn portal_begin(id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
    let mut all = OWNED.lock().ok()?;
    let o = all.iter_mut().find(|o| o.id as u64 == id)?;
    if o.ax.flag("AXMinimized") {
        o.ax.set_flag("AXMinimized", false);
    }
    let b = r_of(macwin::info(o.id)?.bounds);
    let r = mirror::slide_in(o.work, b.w, b.h);
    // A picture of the screen as it is goes over that spot first (above ordinary windows,
    // clicks pass through), then the window slides in under it: unseen on this screen.
    let shot = macwin::screen(macwin::R {
        x: r.x as f64,
        y: r.y as f64,
        w: r.w as f64,
        h: r.h as f64,
    });
    if let Some(pic) = shot {
        crate::macoverlay::veil(Some((r, pic)));
    }
    o.ax.move_to(r.x as f64, r.y as f64);
    // To the front, so the clicks land on it.
    o.ax.raise();
    macwin::bring_forward(o.pid);
    o.slid = Some(r);
    o.seen = (r.x, r.y);
    o.dragging = false;
    set_portal_rect(r);
    PORTAL_ID.store(id, Relaxed);
    PORTAL_ON.store(true, Relaxed);
    Some((r.x + rx.clamp(0, r.w - 1), r.y + ry.clamp(0, r.h - 1)))
}

pub fn portal_end() {
    if !PORTAL_ON.swap(false, Relaxed) {
        return;
    }
    portal_over();
    let id = PORTAL_ID.load(Relaxed);
    if let Ok(mut all) = OWNED.lock() {
        if let Some(o) = all.iter_mut().find(|o| o.id as u64 == id) {
            let size = macwin::info(o.id)
                .map(|w| r_of(w.bounds))
                .map(|b| (b.w, b.h));
            if let Some(size) = size {
                park(o, size);
            }
        }
    }
}

pub fn portal_outside() -> Option<(i32, i32)> {
    if !PORTAL_ON.load(Relaxed) || crate::mac::buttons_down_pub() {
        return None;
    }
    let r = portal_rect();
    let (x, y) = cursor();
    (!r.contains(x, y)).then_some((x - r.x, y - r.y))
}

pub enum Hook {
    Pass,
    Stay,
    Cross(CaptureEvent),
}

/// What the event tap does with a move of this computer's own mouse.
pub fn hook_move(x: i32, y: i32, left_held: bool) -> Hook {
    if PORTAL_ON.load(Relaxed) {
        let r = portal_rect();
        if left_held || r.contains(x, y) {
            return Hook::Stay;
        }
        return Hook::Cross(CaptureEvent::PortalOut {
            rx: x - r.x,
            ry: y - r.y,
        });
    }
    if left_held || crate::macspan::showing() {
        return Hook::Pass;
    }
    match mirror_under(x, y) {
        Some((id, r)) => {
            crate::macoverlay::mirror_raise(id);
            Hook::Cross(CaptureEvent::PortalIn {
                id,
                rx: x - r.x,
                ry: y - r.y,
            })
        }
        None => Hook::Pass,
    }
}

fn pictures() {
    let mut prints: HashMap<WindowId, u64> = HashMap::new();
    loop {
        let t = Instant::now();
        let ids: Vec<WindowId> = OWNED
            .lock()
            .map(|all| all.iter().map(|o| o.id).collect())
            .unwrap_or_default();
        for id in ids {
            look_after(id, &mut prints);
        }
        // Smooth while someone is using a window through its mirror, calmer otherwise.
        let every = if PORTAL_ON.load(Relaxed) {
            mirror::FRAME_EVERY_ACTIVE_MS
        } else {
            mirror::FRAME_EVERY_MS
        };
        let left = Duration::from_millis(every).saturating_sub(t.elapsed());
        std::thread::sleep(left.max(Duration::from_millis(8)));
    }
}

fn look_after(wid: WindowId, prints: &mut HashMap<WindowId, u64>) {
    let id = wid as u64;
    let Some(info) = macwin::info(wid) else {
        // Closed.
        if let Ok(mut all) = OWNED.lock() {
            all.retain(|o| o.id != wid);
        }
        if PORTAL_ID.load(Relaxed) == id && PORTAL_ON.swap(false, Relaxed) {
            portal_over();
        }
        prints.remove(&wid);
        emit(CaptureEvent::MirrorGone { id });
        return;
    };
    let b = r_of(info.bounds);
    let held = crate::mac::buttons_down_pub();
    if let Ok(mut all) = OWNED.lock() {
        if let Some(o) = all.iter_mut().find(|o| o.id == wid) {
            if std::mem::take(&mut o.fresh) {
                prints.remove(&wid);
            }
            // Full screen or minimised here: nothing to show for now.
            if o.ax.flag("AXMinimized") {
                return;
            }
            if o.ax.flag("AXFullScreen") {
                o.ax.set_flag("AXFullScreen", false);
            }
            match o.slid {
                Some(_) => {
                    let (dx, dy) = (b.x - o.seen.0, b.y - o.seen.1);
                    if held && (dx != 0 || dy != 0) {
                        o.dragging = true;
                        emit(CaptureEvent::MirrorMoved {
                            id,
                            dx,
                            dy,
                            done: false,
                        });
                    }
                    o.seen = (b.x, b.y);
                    if o.dragging && !held {
                        o.dragging = false;
                        emit(CaptureEvent::MirrorMoved {
                            id,
                            dx: 0,
                            dy: 0,
                            done: true,
                        });
                        let r = mirror::slide_in(o.work, b.w, b.h);
                        o.ax.move_to(r.x as f64, r.y as f64);
                        let (cx, cy) = cursor();
                        crate::mac::warp_pub((cx + r.x - b.x) as f64, (cy + r.y - b.y) as f64);
                        o.slid = Some(r);
                        o.seen = (r.x, r.y);
                        if PORTAL_ID.load(Relaxed) == id {
                            set_portal_rect(r);
                        }
                    } else if PORTAL_ID.load(Relaxed) == id {
                        o.slid = Some(b);
                        set_portal_rect(b);
                    }
                }
                None => {
                    if !held && (b.x, b.y) != o.parked {
                        park(o, (b.w, b.h));
                    }
                }
            }
        }
    }
    let Some((px, cw, ch)) = macwin::picture(wid) else {
        return;
    };
    let fp = mirror::fingerprint(&px);
    if prints.get(&wid) == Some(&fp) {
        return;
    }
    prints.insert(wid, fp);
    let (sw, sh) = mirror::frame_size(id, cw, ch);
    let small = if (sw, sh) == (cw, ch) {
        px
    } else {
        span::scale4(&px, cw, ch, sw, sh)
    };
    if let Some(jpeg) = span::encode_jpeg(&small, sw, sh, mirror::FRAME_QUALITY) {
        // The size in points: how big it is on screen.
        emit(CaptureEvent::MirrorFrame {
            id,
            w: b.w as u32,
            h: b.h as u32,
            jpeg,
        });
    }
}

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
                shape: 0,
            });
        }
    }
}

// ================================================================== the mirror end

static ENTRIES: Mutex<Vec<(u64, Edge)>> = Mutex::new(Vec::new());

fn mirror_under(x: i32, y: i32) -> Option<(u64, Rect)> {
    let (id, num, r) = {
        let m = crate::macoverlay::MIRRORS.try_lock().ok()?;
        *m.iter().find(|e| e.2.contains(x, y))?
    };
    // Covered by another window here? Then it isn't the mirror under the pointer.
    let top = macwin::top_at(x as f64, y as f64)?;
    (top.id == num).then_some((id, r))
}

pub fn create(id: u64, w: u32, h: u32, entry: Edge) -> bool {
    let s = crate::mac::desktop_pub();
    let (px, py) = crate::macoverlay::current_picture().unwrap_or((
        s.x + (s.width - w as i32) / 2,
        s.y + (s.height - h as i32) / 2,
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
    if let Ok(mut e) = ENTRIES.lock() {
        e.retain(|x| x.0 != id);
        e.push((id, entry));
    }
    crate::macoverlay::mirror_open(id, rect);
    crate::macoverlay::hide(0);
    true
}

pub fn frame(id: u64, w: u32, h: u32, jpeg: Vec<u8>) {
    crate::macoverlay::mirror_picture(id, w, h, jpeg);
}

pub fn close(id: u64) {
    if let Ok(mut e) = ENTRIES.lock() {
        e.retain(|x| x.0 != id);
    }
    crate::macoverlay::mirror_close(id);
}

pub fn move_by(id: u64, dx: i32, dy: i32, done: bool) -> Option<f32> {
    let rect = {
        let mut m = crate::macoverlay::MIRRORS.lock().ok()?;
        let e = m.iter_mut().find(|e| e.0 == id)?;
        e.2.x += dx;
        e.2.y += dy;
        e.2
    };
    crate::macoverlay::mirror_move(id, rect);
    if !done {
        return None;
    }
    let entry = ENTRIES.lock().ok()?.iter().find(|e| e.0 == id)?.1;
    let scr = crate::mac::desktop_pub();
    match mirror::home_along(rect, scr, entry) {
        Some(along) => {
            close(id);
            Some(along)
        }
        None => {
            // Let go anywhere else: it stays where it can be reached, never off screen.
            let kept = mirror::first_place(rect, scr);
            if kept != rect {
                if let Ok(mut m) = crate::macoverlay::MIRRORS.lock() {
                    if let Some(e) = m.iter_mut().find(|e| e.0 == id) {
                        e.2 = kept;
                    }
                }
                crate::macoverlay::mirror_move(id, kept);
            }
            None
        }
    }
}

pub fn at_cursor() -> Option<(u64, i32, i32)> {
    if crate::mac::buttons_down_pub() {
        return None;
    }
    let (x, y) = cursor();
    let (id, r) = mirror_under(x, y)?;
    crate::macoverlay::mirror_raise(id);
    Some((id, x - r.x, y - r.y))
}

pub fn point(id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
    let m = crate::macoverlay::MIRRORS.lock().ok()?;
    let e = m.iter().find(|e| e.0 == id)?;
    Some((e.2.x + rx, e.2.y + ry))
}

pub fn cursor_on(id: u64, at: Option<(i32, i32)>) {
    if at.is_none() {
        span::set_shape(span::shape::ARROW);
    }
    crate::macoverlay::cursor_overlay(at.and_then(|(x, y)| point(id, x, y)));
}
