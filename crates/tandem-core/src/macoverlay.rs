//! What CubeCursor puts on the Mac's screen itself: the picture of a window coming over
//! from another computer, the stand-in pointer, the glow at the edge, and the mirrors of
//! second-monitor mode.
//!
//! AppKit windows only live on the main thread, which Tauri runs. Anything that wants
//! one changed queues a command for a worker thread, which does the slow part (decoding
//! pictures) and then hands the window work to the main thread.

use crate::mirror::Rect;
use crate::platform::{Screen, SpanView};
use crate::proto::Edge;
use crate::span;
use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::data_provider::CGDataProvider;
use core_graphics::display::CGDisplay;
use core_graphics::image::CGImage;
use dispatch2::DispatchQueue;
use foreign_types::ForeignType;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSWindow, NSWindowCollectionBehavior, NSWindowLevel,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::{Duration, Instant};

/// Above everything, the menu bar and full-screen apps included.
const OVER_ALL: NSWindowLevel = 101;

// ------------------------------------------------------------------ pictures in windows

/// A picture for a window: premultiplied BGRA, width, height.
pub type Pic = (Vec<u8>, u32, u32);

fn cg_image(px: Vec<u8>, w: u32, h: u32) -> Option<CGImage> {
    if w == 0 || h == 0 || px.len() < (w * h * 4) as usize {
        return None;
    }
    let provider = CGDataProvider::from_buffer(Arc::new(px));
    Some(CGImage::new(
        w as usize,
        h as usize,
        8,
        32,
        (w * 4) as usize,
        &CGColorSpace::create_device_rgb(),
        kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
        &provider,
        true,
        0,
    ))
}

/// The main display's height: AppKit counts up from the bottom, everything else down
/// from the top.
fn flip_h() -> f64 {
    CGDisplay::main().bounds().size.height
}

fn ns_rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, flip_h() - (y + h)), NSSize::new(w, h))
}

/// A borderless window for one of our pictures. `through`: clicks go to what is below.
fn make_window(
    mtm: MainThreadMarker,
    r: NSRect,
    through: bool,
    level: NSWindowLevel,
) -> Retained<NSWindow> {
    let win = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            r,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { win.setReleasedWhenClosed(false) };
    win.setOpaque(false);
    win.setBackgroundColor(Some(&NSColor::clearColor()));
    win.setHasShadow(!through);
    win.setIgnoresMouseEvents(through);
    win.setLevel(level);
    win.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    if let Some(view) = win.contentView() {
        view.setWantsLayer(true);
        unsafe {
            let layer: *mut AnyObject = msg_send![&*view, layer];
            if !layer.is_null() {
                let gravity = NSString::from_str("resize");
                let _: () = msg_send![layer, setContentsGravity: &*gravity];
            }
        }
    }
    win
}

/// Puts a picture in window `win`.
fn set_picture(win: &NSWindow, img: &CGImage) {
    let Some(view) = win.contentView() else {
        return;
    };
    unsafe {
        let layer: *mut AnyObject = msg_send![&*view, layer];
        if layer.is_null() {
            return;
        }
        let obj = img.as_ptr() as *const AnyObject;
        let _: () = msg_send![layer, setContents: obj];
    }
}

// ------------------------------------------------------------------ the main thread's side

#[derive(Default)]
struct Ui {
    pic: Option<Retained<NSWindow>>,
    arrow: Option<Retained<NSWindow>>,
    glow: Option<Retained<NSWindow>>,
    mirrors: HashMap<u64, Retained<NSWindow>>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

/// Runs `f` on the main thread with the windows.
fn on_main(f: impl FnOnce(MainThreadMarker, &mut Ui) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        UI.with(|u| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                f(mtm, &mut u.borrow_mut())
            }));
        });
    });
}

/// Images aren't `Send`: hand pixels over and make the image on the main thread.
fn show_in(
    which: fn(&mut Ui) -> &mut Option<Retained<NSWindow>>,
    r: (f64, f64, f64, f64),
    pic: Option<Pic>,
    alpha: f64,
) {
    on_main(move |mtm, ui| {
        let rect = ns_rect(r.0, r.1, r.2, r.3);
        let slot = which(ui);
        let win = slot.get_or_insert_with(|| make_window(mtm, rect, true, OVER_ALL));
        win.setFrame_display(rect, false);
        if let Some((px, w, h)) = pic {
            if let Some(img) = cg_image(px, w, h) {
                set_picture(win, &img);
            }
        }
        win.setAlphaValue(alpha);
        win.orderFrontRegardless();
    });
}

fn hide_in(which: fn(&mut Ui) -> &mut Option<Retained<NSWindow>>) {
    on_main(move |_, ui| {
        if let Some(w) = which(ui).as_ref() {
            w.orderOut(None);
        }
    });
}

// ------------------------------------------------------------------ the worker

enum Cmd {
    Show(SpanView),
    Frame(Vec<u8>),
    Hide(u32),
    Glow(Edge, f32, [u8; 3]),
}

#[derive(Default)]
struct Work {
    q: VecDeque<Cmd>,
}

static WORK: Mutex<Work> = Mutex::new(Work { q: VecDeque::new() });
static WAKE: Condvar = Condvar::new();
static START: Once = Once::new();

fn post(c: Cmd) {
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-overlay".into())
            .spawn(worker);
    });
    if let Ok(mut w) = WORK.lock() {
        let same = |a: &Cmd, b: &Cmd| {
            matches!(
                (a, b),
                (Cmd::Show(_), Cmd::Show(_)) | (Cmd::Frame(_), Cmd::Frame(_))
            )
        };
        w.q.retain(|old| !same(old, &c));
        w.q.push_back(c);
        WAKE.notify_one();
    }
}

fn desktop() -> Screen {
    crate::mac::desktop_pub()
}

fn worker() {
    let mut view: Option<SpanView> = None;
    let mut frame: Option<Pic> = None;
    let mut hide_at: Option<Instant> = None;
    let mut glow: Option<(Instant, (f64, f64, f64, f64))> = None;
    loop {
        let cmds: Vec<Cmd> = {
            let Ok(mut w) = WORK.lock() else {
                return;
            };
            let timeout = if glow.is_some() || hide_at.is_some() {
                Duration::from_millis(15)
            } else {
                Duration::from_secs(3600)
            };
            if w.q.is_empty() {
                w = match WAKE.wait_timeout(w, timeout) {
                    Ok((g, _)) => g,
                    Err(_) => return,
                };
            }
            w.q.drain(..).collect()
        };
        let mut dirty = false;
        for c in cmds {
            match c {
                Cmd::Show(v) => {
                    if view.map(|o| (o.w, o.h, o.gx, o.gy)) != Some((v.w, v.h, v.gx, v.gy)) {
                        dirty = true;
                    }
                    view = Some(v);
                    hide_at = None;
                }
                Cmd::Frame(j) => {
                    if let Some(f) = span::decode_jpeg(&j) {
                        frame = Some(f);
                        dirty = true;
                    }
                }
                Cmd::Hide(0) => {
                    view = None;
                    frame = None;
                    hide_at = None;
                    hide_in(|u| &mut u.pic);
                }
                Cmd::Hide(ms) => hide_at = Some(Instant::now() + Duration::from_millis(ms as u64)),
                Cmd::Glow(edge, frac, rgb) => {
                    let (x, y, w, h, px) = span::glow_strip(desktop(), edge, frac, rgb);
                    let r = (x as f64, y as f64, w as f64, h as f64);
                    show_in(|u| &mut u.glow, r, Some((px, w as u32, h as u32)), 0.0);
                    glow = Some((Instant::now(), r));
                }
            }
        }
        if hide_at.is_some_and(|t| Instant::now() >= t) {
            hide_at = None;
            view = None;
            frame = None;
            hide_in(|u| &mut u.pic);
        }
        if let Some(v) = view {
            let (x, y) = span::place(&v, desktop());
            let pic = dirty.then(|| {
                let f = frame.as_ref().map(|(p, w, h)| (p.as_slice(), *w, *h));
                (span::compose(f, v.w, v.h, v.gx, v.gy), v.w, v.h)
            });
            show_in(
                |u| &mut u.pic,
                (x as f64, y as f64, v.w as f64, v.h as f64),
                pic,
                1.0,
            );
        }
        // The glow: quick to come up, a slower fade.
        if let Some((t0, _)) = glow {
            let t = t0.elapsed().as_secs_f32();
            let (fade_in, fade_out) = (0.09, 0.6);
            if t >= fade_in + fade_out {
                glow = None;
                hide_in(|u| &mut u.glow);
            } else {
                let a = if t < fade_in {
                    t / fade_in
                } else {
                    (1.0 - (t - fade_in) / fade_out).max(0.0).powi(2)
                } as f64;
                on_main(move |_, ui| {
                    if let Some(w) = ui.glow.as_ref() {
                        w.setAlphaValue(a);
                    }
                });
            }
        }
    }
}

// ------------------------------------------------------------------ the picture coming over

pub fn show(v: SpanView) {
    post(Cmd::Show(v));
}
pub fn frame(jpeg: Vec<u8>) {
    post(Cmd::Frame(jpeg));
}
pub fn hide(after_ms: u32) {
    post(Cmd::Hide(after_ms));
}
pub fn glow(edge: Edge, frac: f32, rgb: [u8; 3]) {
    post(Cmd::Glow(edge, frac, rgb));
}

static ARROW_SHOWN: AtomicBool = AtomicBool::new(false);

/// The stand-in pointer at (`x`, `y`), or gone.
pub fn cursor_overlay(at: Option<(i32, i32)>) {
    match at {
        Some((x, y)) => {
            let first = !ARROW_SHOWN.swap(true, Relaxed);
            let (px, aw, ah, tx, ty) = span::arrow();
            let r = ((x - tx) as f64, (y - ty) as f64, aw as f64, ah as f64);
            show_in(|u| &mut u.arrow, r, first.then_some((px, aw, ah)), 1.0);
        }
        None => {
            if ARROW_SHOWN.swap(false, Relaxed) {
                hide_in(|u| &mut u.arrow);
            }
        }
    }
}

/// Where the picture of a window coming over is now, and the picture: what a mirror
/// starts from.
pub fn current_picture() -> Option<(i32, i32)> {
    LAST_PLACE.lock().ok().and_then(|p| *p)
}

static LAST_PLACE: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// Remembers where the last picture went (for `current_picture`).
pub fn note_place(v: &SpanView) {
    let (x, y) = span::place(v, desktop());
    if let Ok(mut p) = LAST_PLACE.lock() {
        *p = Some((x, y));
    }
}

// ------------------------------------------------------------------ mirrors

/// Mirror windows, for the event tap: id, the window server's number, where.
pub static MIRRORS: Mutex<Vec<(u64, u32, Rect)>> = Mutex::new(Vec::new());

fn set_mirror_number(id: u64, num: u32) {
    if let Ok(mut m) = MIRRORS.lock() {
        if let Some(e) = m.iter_mut().find(|e| e.0 == id) {
            e.1 = num;
        }
    }
}

pub fn mirror_open(id: u64, r: Rect) {
    if let Ok(mut m) = MIRRORS.lock() {
        m.retain(|e| e.0 != id);
        m.push((id, 0, r));
    }
    on_main(move |mtm, ui| {
        let rect = ns_rect(r.x as f64, r.y as f64, r.w as f64, r.h as f64);
        let win = make_window(mtm, rect, false, 0);
        win.setOpaque(true);
        win.setBackgroundColor(Some(&NSColor::blackColor()));
        win.orderFrontRegardless();
        set_mirror_number(id, win.windowNumber() as u32);
        ui.mirrors.insert(id, win);
    });
}

pub fn mirror_picture(id: u64, w: u32, h: u32, jpeg: Vec<u8>) {
    // Decoding takes a moment: not on the engine's thread, not on the main thread.
    std::thread::spawn(move || {
        let Some((px, fw, fh)) = span::decode_jpeg(&jpeg) else {
            return;
        };
        let resized = MIRRORS.lock().ok().and_then(|mut m| {
            let e = m.iter_mut().find(|e| e.0 == id)?;
            let changed = (e.2.w, e.2.h) != (w as i32, h as i32);
            e.2.w = w as i32;
            e.2.h = h as i32;
            changed.then_some(e.2)
        });
        on_main(move |_, ui| {
            let Some(win) = ui.mirrors.get(&id) else {
                return;
            };
            if let Some(r) = resized {
                win.setFrame_display(
                    ns_rect(r.x as f64, r.y as f64, r.w as f64, r.h as f64),
                    false,
                );
            }
            if let Some(img) = cg_image(px, fw, fh) {
                set_picture(win, &img);
            }
        });
    });
}

pub fn mirror_move(id: u64, r: Rect) {
    on_main(move |_, ui| {
        if let Some(win) = ui.mirrors.get(&id) {
            win.setFrame_display(
                ns_rect(r.x as f64, r.y as f64, r.w as f64, r.h as f64),
                false,
            );
        }
    });
}

pub fn mirror_raise(id: u64) {
    on_main(move |_, ui| {
        if let Some(win) = ui.mirrors.get(&id) {
            win.orderFrontRegardless();
        }
    });
}

pub fn mirror_close(id: u64) {
    if let Ok(mut m) = MIRRORS.lock() {
        m.retain(|e| e.0 != id);
    }
    on_main(move |_, ui| {
        if let Some(win) = ui.mirrors.remove(&id) {
            win.orderOut(None);
            win.close();
        }
    });
}
