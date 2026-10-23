//! The Mac input layer: an event tap watches the mouse and keyboard (and swallows them
//! while they belong to another computer), synthetic events move the cursor and type
//! when another computer is in control, and the general pasteboard is the clipboard.
//!
//! macOS asks the person to allow two things in System Settings › Privacy & Security
//! before any of this works: Accessibility (to control the computer) and Input
//! Monitoring (to see the keyboard and mouse). Until then the tap can't be created;
//! it is tried again every few seconds, so allowing it later starts it without a
//! restart of the app.

use crate::keymap::{mac_modifier_flag, mac_to_win, win_to_mac};
use crate::platform::*;
use crate::proto::{Button, Edge, InputEvent};
use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use foreign_types::ForeignType;
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed,
};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

// ---------------------------------------------------------------- system calls

type TapCallback = unsafe extern "C" fn(
    proxy: *const c_void,
    etype: u32,
    event: core_graphics::sys::CGEventRef,
    info: *mut c_void,
) -> core_graphics::sys::CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
}

/// Both permissions macOS needs for this app have been given.
pub fn permissions_granted() -> bool {
    unsafe {
        AXIsProcessTrusted() && CGPreflightListenEventAccess() && CGPreflightPostEventAccess()
    }
}

/// Shows macOS' own prompts (once) that lead to the two settings.
pub fn request_permissions() {
    unsafe {
        if !CGPreflightListenEventAccess() {
            CGRequestListenEventAccess();
        }
        if !CGPreflightPostEventAccess() {
            CGRequestPostEventAccess();
        }
    }
}

// ---------------------------------------------------------------- shared state

static SINK: OnceLock<UnboundedSender<CaptureEvent>> = OnceLock::new();
static CAPTURING: AtomicBool = AtomicBool::new(false);
static EDGES: AtomicU8 = AtomicU8::new(0);
static AT_EDGE: AtomicU32 = AtomicU32::new(0);
static AT_BIT: AtomicU8 = AtomicU8::new(0);
static EDGE_SINCE_MS: AtomicU64 = AtomicU64::new(0);
static SWITCH_DELAY_MS: AtomicU32 = AtomicU32::new(0);
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// Escape presses while the keyboard is on another computer (three quick ones = way out).
static ESC_TIMES: Mutex<Vec<Instant>> = Mutex::new(Vec::new());
/// Fractions of a wheel notch not sent yet (trackpads scroll in small steps).
static SCROLL_ACC_Y: AtomicI64 = AtomicI64::new(0);
static SCROLL_ACC_X: AtomicI64 = AtomicI64::new(0);
/// Dragging windows across the edge does something (moving apps or second monitor).
static SPANS: AtomicBool = AtomicBool::new(false);
static DRAG_FILES: AtomicBool = AtomicBool::new(true);
static GAME_MODE: AtomicBool = AtomicBool::new(true);
/// A full-screen app (a game, a film) is in front.
static GAME_ACTIVE: AtomicBool = AtomicBool::new(false);
/// The physical left button is down (as this computer's own tap saw it).
static LEFT_HELD: AtomicBool = AtomicBool::new(false);
/// The drag pasteboard's counter when the button went down: if it changed, files are
/// being dragged (Finder puts them there when a drag starts).
static DRAG_COUNT: AtomicI64 = AtomicI64::new(-1);
/// Whether we hid the pointer (macOS counts hides: show exactly as often).
static POINTER_HIDDEN: AtomicBool = AtomicBool::new(false);
/// Held still for a window being moved across (no swallowing to another computer).
static HOLDING: AtomicBool = AtomicBool::new(false);

/// Marks the events we post ourselves, so our own tap lets them through untouched.
const MAGIC: i64 = 0x7A6E_4443; // "CuCu"
/// Mouse moves pushing against an edge before the cursor crosses.
const PUSH_EVENTS: u32 = 2;

const EDGE_LEFT: u8 = 1;
const EDGE_RIGHT: u8 = 2;
const EDGE_TOP: u8 = 4;
const EDGE_BOTTOM: u8 = 8;
const EDGE_TL: u8 = 16;
const EDGE_TR: u8 = 32;
const EDGE_BL: u8 = 64;
const EDGE_BR: u8 = 128;

fn edge_mask(edges: &[Edge]) -> u8 {
    edges.iter().fold(0, |m, e| {
        m | match e {
            Edge::Left => EDGE_LEFT,
            Edge::Right => EDGE_RIGHT,
            Edge::Top => EDGE_TOP,
            Edge::Bottom => EDGE_BOTTOM,
            Edge::TopLeft => EDGE_TL,
            Edge::TopRight => EDGE_TR,
            Edge::BottomLeft => EDGE_BL,
            Edge::BottomRight => EDGE_BR,
        }
    })
}

fn emit(ev: CaptureEvent) {
    if let Some(tx) = SINK.get() {
        let _ = tx.send(ev);
    }
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The whole desktop (all displays), in the points macOS positions the cursor in.
fn desktop() -> Screen {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    let mut minx = f64::MAX;
    let mut miny = f64::MAX;
    let mut maxx = f64::MIN;
    let mut maxy = f64::MIN;
    for id in ids {
        let b = CGDisplay::new(id).bounds();
        minx = minx.min(b.origin.x);
        miny = miny.min(b.origin.y);
        maxx = maxx.max(b.origin.x + b.size.width);
        maxy = maxy.max(b.origin.y + b.size.height);
    }
    if minx > maxx {
        let b = CGDisplay::main().bounds();
        return Screen {
            x: 0,
            y: 0,
            width: b.size.width as i32,
            height: b.size.height as i32,
        };
    }
    Screen {
        x: minx.round() as i32,
        y: miny.round() as i32,
        width: (maxx - minx).round() as i32,
        height: (maxy - miny).round() as i32,
    }
}

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

fn cursor() -> CGPoint {
    source()
        .and_then(|s| CGEvent::new(s).ok())
        .map(|e| e.location())
        .unwrap_or(CGPoint::new(0.0, 0.0))
}

fn warp(x: f64, y: f64) {
    let _ = CGDisplay::warp_mouse_cursor_position(CGPoint::new(x, y));
}

/// Hides (or shows again) the pointer, once whatever asks how often.
fn pointer_hidden(on: bool) {
    if POINTER_HIDDEN.swap(on, Relaxed) == on {
        return;
    }
    let main = CGDisplay::main();
    if on {
        let _ = main.hide_cursor();
    } else {
        let _ = main.show_cursor();
    }
}

/// The pointer stays put (macOS still reports how far the mouse moves) and hidden, while
/// capturing or holding.
fn pointer_still(on: bool) {
    let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(!on);
    pointer_hidden(on);
}

fn set_capturing(on: bool) {
    if CAPTURING.swap(on, Relaxed) == on {
        return;
    }
    // While the mouse belongs to another computer, the cursor here stays put.
    pointer_still(on || HOLDING.load(Relaxed));
}

// ---- for the other Mac modules (window drag, mirrors, glass)

pub(crate) fn emit_pub(ev: CaptureEvent) {
    emit(ev);
}
pub(crate) fn desktop_pub() -> Screen {
    desktop()
}
pub(crate) fn cursor_pub() -> (i32, i32) {
    let p = cursor();
    (p.x.round() as i32, p.y.round() as i32)
}
pub(crate) fn warp_pub(x: f64, y: f64) {
    warp(x, y);
}
/// A mouse button is physically down.
pub(crate) fn buttons_down_pub() -> bool {
    unsafe { CGEventSourceButtonState(1, 0) || CGEventSourceButtonState(1, 1) }
}
/// Ends macOS' own window drag: a left button-up of ours (our tap lets it through).
pub(crate) fn post_left_up_pub() {
    let Some(src) = source() else { return };
    if let Ok(e) = CGEvent::new_mouse_event(src, CGEventType::LeftMouseUp, cursor(), CGMouseButton::Left) {
        e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, MAGIC);
        e.post(CGEventTapLocation::HID);
    }
}
fn post_key_pub(code: u16) {
    let Some(src) = source() else { return };
    for down in [true, false] {
        if let Ok(e) = CGEvent::new_keyboard_event(src.clone(), code, down) {
            e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, MAGIC);
            e.post(CGEventTapLocation::HID);
        }
    }
}
/// Holds the pointer still and hidden while CubeCursor moves a window across with it.
pub(crate) fn hold_pointer_pub(on: bool) {
    HOLDING.store(on, Relaxed);
    pointer_still(on || CAPTURING.load(Relaxed));
}

/// The window server's number of one of our windows (Tauri gives its `NSWindow`).
pub fn window_number(ns_window: *mut c_void) -> u32 {
    if ns_window.is_null() {
        return 0;
    }
    unsafe {
        let n: isize = objc2::msg_send![ns_window as *mut objc2::runtime::AnyObject, windowNumber];
        n as u32
    }
}

/// Liquid Glass: what is on screen in the given rectangle (points) underneath our window
/// `below`, at half size, as a `data:` JPEG.
pub fn backdrop_below(below: u32, x: f64, y: f64, w: f64, h: f64) -> Option<String> {
    use base64::Engine as _;
    let (px, pw, ph) = crate::macwin::below(below, crate::macwin::R { x, y, w, h })?;
    let (sw, sh) = ((pw / 2).max(1), (ph / 2).max(1));
    let small = crate::span::scale4(&px, pw, ph, sw, sh);
    let jpeg = crate::span::encode_jpeg(&small, sw, sh, 70)?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(jpeg)
    ))
}

/// Liquid Glass, fast path: what is under our window `below` in the rectangle (points),
/// shrunk to `ow` x `oh` (it gets blurred anyway), as RGBA.
pub fn backdrop_rgba(below: u32, x: f64, y: f64, w: f64, h: f64, ow: u32, oh: u32) -> Option<Vec<u8>> {
    let (px, pw, ph) = crate::macwin::below(below, crate::macwin::R { x, y, w, h })?;
    let mut small = crate::span::scale4(&px, pw, ph, ow.max(1), oh.max(1));
    for p in small.chunks_exact_mut(4) {
        p.swap(0, 2);
        p[3] = 255;
    }
    Some(small)
}

/// CubeCursor is closing: windows kept here for a mirror come back on screen, and the
/// pointer is shown again.
pub fn on_exit() {
    crate::macmirror::release_all();
    pointer_hidden(false);
}

/// The main display, at half size, as a `data:` JPEG (a screenshot for feedback).
pub fn screenshot() -> Option<String> {
    use base64::Engine as _;
    let b = CGDisplay::main().bounds();
    let r = crate::macwin::R { x: b.origin.x, y: b.origin.y, w: b.size.width, h: b.size.height };
    let (px, pw, ph) = crate::macwin::screen(r)?;
    let (sw, sh) = crate::span::fit(pw, ph, 1600, 1000);
    let small = crate::span::scale4(&px, pw, ph, sw, sh);
    let jpeg = crate::span::encode_jpeg(&small, sw, sh, 70)?;
    Some(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg)))
}

/// Liquid Glass needs Screen Recording permission on the Mac: ask for it.
pub fn ask_screen_recording() {
    crate::macwin::ask_record();
}

/// The files of the drag in progress, if one started since the button went down.
fn dragged_files() -> Option<Vec<PathBuf>> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSPasteboard, NSPasteboardNameDrag, NSPasteboardTypeFileURL};
    use objc2_foundation::NSURL;
    autoreleasepool(|_| unsafe {
        let pb = NSPasteboard::pasteboardWithName(NSPasteboardNameDrag);
        let at_press = DRAG_COUNT.load(Relaxed);
        if at_press < 0 || pb.changeCount() as i64 == at_press {
            return None;
        }
        let mut files = Vec::new();
        for item in pb.pasteboardItems()?.iter() {
            if let Some(s) = item.stringForType(NSPasteboardTypeFileURL) {
                if let Some(p) = NSURL::URLWithString(&s).and_then(|u| u.path()) {
                    files.push(PathBuf::from(p.to_string()));
                }
            }
        }
        (!files.is_empty()).then_some(files)
    })
}

/// The button went down: note the window under it and the drag pasteboard.
fn note_press(x: f64, y: f64) {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSPasteboard, NSPasteboardNameDrag};
    let n = autoreleasepool(|_| unsafe { NSPasteboard::pasteboardWithName(NSPasteboardNameDrag).changeCount() as i64 });
    DRAG_COUNT.store(n, Relaxed);
    if SPANS.load(Relaxed) {
        crate::macspan::pressed(x, y);
    }
}

/// The four sides among the active edges.
fn side_edges() -> Vec<Edge> {
    let m = EDGES.load(Relaxed);
    [(EDGE_LEFT, Edge::Left), (EDGE_RIGHT, Edge::Right), (EDGE_TOP, Edge::Top), (EDGE_BOTTOM, Edge::Bottom)]
        .into_iter()
        .filter(|(b, _)| m & b != 0)
        .map(|(_, e)| e)
        .collect()
}

/// Watches for a full-screen app in front (game mode).
fn game_watcher() {
    loop {
        std::thread::sleep(Duration::from_millis(600));
        if !GAME_MODE.load(Relaxed) {
            GAME_ACTIVE.store(false, Relaxed);
            continue;
        }
        let displays: Vec<_> = CGDisplay::active_displays()
            .unwrap_or_default()
            .into_iter()
            .map(|id| CGDisplay::new(id).bounds())
            .collect();
        let me = std::process::id() as i32;
        let front = crate::macwin::on_screen().into_iter().find(|w| w.layer == 0 && w.pid != me);
        let full = front.is_some_and(|w| {
            displays.iter().any(|d| {
                (w.bounds.x - d.origin.x).abs() < 1.0
                    && (w.bounds.y - d.origin.y).abs() < 1.0
                    && (w.bounds.w - d.size.width).abs() < 1.0
                    && (w.bounds.h - d.size.height).abs() < 1.0
            })
        });
        GAME_ACTIVE.store(full, Relaxed);
    }
}

// ---------------------------------------------------------------- the event tap

fn tap_thread() {
    request_permissions();
    let mut told = false;
    loop {
        let mask: u64 = [
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::OtherMouseDragged,
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
            CGEventType::ScrollWheel,
        ]
        .iter()
        .fold(0u64, |m, t| m | (1u64 << (*t as u32)));
        let port = unsafe {
            CGEventTapCreate(
                CGEventTapLocation::HID as u32,
                0, // head insert
                0, // active: may swallow events
                mask,
                tap_callback,
                std::ptr::null_mut(),
            )
        };
        if port.is_null() {
            if !told {
                log::warn!("mac: no permission to watch the keyboard and mouse yet (Accessibility / Input Monitoring)");
                told = true;
            }
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        log::info!("mac: keyboard and mouse tap started");
        TAP.store(port as *mut c_void, Relaxed);
        unsafe {
            let port = CFMachPort::wrap_under_create_rule(port);
            let Ok(src) = port.create_runloop_source(0) else {
                log::error!("mac: cannot attach the tap");
                return;
            };
            let rl = CFRunLoop::get_current();
            rl.add_source(&src, kCFRunLoopCommonModes);
            CGEventTapEnable(port.as_concrete_TypeRef(), true);
            CFRunLoop::run_current();
        }
        return;
    }
}

unsafe extern "C" fn tap_callback(
    _proxy: *const c_void,
    etype: u32,
    event: core_graphics::sys::CGEventRef,
    _info: *mut c_void,
) -> core_graphics::sys::CGEventRef {
    // macOS turns a tap off when it is too slow or after some secure input: turn it on.
    if etype == CGEventType::TapDisabledByTimeout as u32
        || etype == CGEventType::TapDisabledByUserInput as u32
    {
        let t = TAP.load(Relaxed);
        if !t.is_null() {
            CGEventTapEnable(t as CFMachPortRef, true);
        }
        return event;
    }
    if event.is_null() {
        return event;
    }
    let ev = ManuallyDrop::new(CGEvent::from_ptr(event));
    let swallow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(etype, &ev)))
        .unwrap_or(false);
    if swallow {
        std::ptr::null_mut()
    } else {
        event
    }
}

/// Returns true to swallow the event (it belongs to another computer).
fn handle(etype: u32, ev: &CGEvent) -> bool {
    if ev.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == MAGIC {
        return false; // one of ours
    }
    let t = etype;
    let is = |k: CGEventType| t == k as u32;
    // A window hanging across an edge: the mouse moves it, nothing goes anywhere else.
    if crate::macspan::ACTIVE.load(Relaxed) && !CAPTURING.load(Relaxed) {
        if is(CGEventType::MouseMoved) || is(CGEventType::LeftMouseDragged) {
            let dx = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32;
            let dy = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32;
            if dx != 0 || dy != 0 {
                crate::macspan::mouse(dx, dy);
            }
        } else if is(CGEventType::LeftMouseUp) {
            LEFT_HELD.store(false, Relaxed);
            if let Some((moved, edge, frac)) = crate::macspan::release() {
                if moved {
                    // The mouse carries on over there.
                    HOLDING.store(false, Relaxed);
                    set_capturing(true);
                    emit(CaptureEvent::Crossed { edge, frac });
                } else {
                    hold_pointer_pub(false);
                }
            }
            crate::macspan::released();
        } else if is(CGEventType::RightMouseDown)
            || (is(CGEventType::KeyDown) && ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) == 0x35)
        {
            crate::macspan::cancel();
        }
        return true;
    }
    if !CAPTURING.load(Relaxed) {
        if is(CGEventType::LeftMouseDown) {
            LEFT_HELD.store(true, Relaxed);
            let p = ev.location();
            note_press(p.x, p.y);
            return false;
        }
        if is(CGEventType::LeftMouseUp) {
            LEFT_HELD.store(false, Relaxed);
            // A window that was going past an edge was let go before the pointer got there.
            if crate::macspan::showing() {
                crate::macspan::release();
            }
            crate::macspan::released();
            DRAG_COUNT.store(-1, Relaxed);
            emit(CaptureEvent::LocalLeftUp);
            return false;
        }
        if is(CGEventType::MouseMoved) || is(CGEventType::LeftMouseDragged) {
            let p = ev.location();
            let (x, y) = (p.x.round() as i32, p.y.round() as i32);
            let held = is(CGEventType::LeftMouseDragged) || LEFT_HELD.load(Relaxed);
            // Second-monitor mode: onto a mirror, or off a window driven here.
            if !GAME_ACTIVE.load(Relaxed) {
                match crate::macmirror::hook_move(x, y, held) {
                    crate::macmirror::Hook::Pass => {}
                    crate::macmirror::Hook::Stay => return false,
                    crate::macmirror::Hook::Cross(e) => {
                        let (cx, cy) = desktop().center();
                        warp(cx as f64, cy as f64);
                        set_capturing(true);
                        emit(e);
                        return true;
                    }
                }
            }
            if is(CGEventType::MouseMoved) {
                return edge_check(ev);
            }
            return drag_edge(ev, x, y);
        }
        return false;
    }
    // Everything physical now goes to the other computer.
    let out = if is(CGEventType::MouseMoved)
        || is(CGEventType::LeftMouseDragged)
        || is(CGEventType::RightMouseDragged)
        || is(CGEventType::OtherMouseDragged)
    {
        let dx = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32;
        let dy = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32;
        (dx != 0 || dy != 0).then_some(InputEvent::MouseMove { dx, dy })
    } else if is(CGEventType::LeftMouseDown) || is(CGEventType::LeftMouseUp) {
        Some(InputEvent::Button {
            button: Button::Left,
            down: is(CGEventType::LeftMouseDown),
        })
    } else if is(CGEventType::RightMouseDown) || is(CGEventType::RightMouseUp) {
        Some(InputEvent::Button {
            button: Button::Right,
            down: is(CGEventType::RightMouseDown),
        })
    } else if is(CGEventType::OtherMouseDown) || is(CGEventType::OtherMouseUp) {
        let button = match ev.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER) {
            3 => Button::X1,
            4 => Button::X2,
            _ => Button::Middle,
        };
        Some(InputEvent::Button {
            button,
            down: is(CGEventType::OtherMouseDown),
        })
    } else if is(CGEventType::ScrollWheel) {
        // Lines, with fractions for trackpads; Windows counts 120 per notch.
        let fy = ev.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1);
        let fx = ev.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2);
        let take = |acc: &AtomicI64, v: f64| {
            let total = acc.load(Relaxed) + (v * 120.0).round() as i64;
            let send = if total.abs() >= 30 { total } else { 0 };
            acc.store(total - send, Relaxed);
            send as i32
        };
        let dy = take(&SCROLL_ACC_Y, fy);
        let dx = take(&SCROLL_ACC_X, -fx);
        (dx != 0 || dy != 0).then_some(InputEvent::Wheel { dx, dy })
    } else if is(CGEventType::KeyDown) || is(CGEventType::KeyUp) {
        let code = ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
        let down = is(CGEventType::KeyDown);
        if code == 0x35 && down && failsafe_pressed() {
            CAPTURING.store(true, Relaxed);
            set_capturing(false);
            emit(CaptureEvent::Failsafe);
            return true;
        }
        mac_to_win(code).map(|(vk, scan, extended)| InputEvent::Key {
            vk,
            scan,
            extended,
            down,
        })
    } else if is(CGEventType::FlagsChanged) {
        let code = ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
        match (mac_modifier_flag(code), mac_to_win(code)) {
            (Some(flag), Some((vk, scan, extended))) => Some(InputEvent::Key {
                vk,
                scan,
                extended,
                down: ev.get_flags().bits() & flag != 0,
            }),
            _ => None,
        }
    } else {
        None
    };
    if let Some(e) = out {
        emit(CaptureEvent::Input(e));
    }
    true
}

/// Three presses of Escape within a second while the keyboard is elsewhere bring the
/// mouse and keyboard back, whatever the other computer is doing.
fn failsafe_pressed() -> bool {
    let mut v = ESC_TIMES.lock().unwrap();
    let now = Instant::now();
    v.retain(|t| now.duration_since(*t) < Duration::from_millis(1000));
    v.push(now);
    if v.len() >= 3 {
        v.clear();
        true
    } else {
        false
    }
}

/// The cursor (not carrying anything) pushing against an edge that leads to another
/// computer: cross over.
fn edge_check(ev: &CGEvent) -> bool {
    let mask = EDGES.load(Relaxed);
    if mask == 0 || GAME_ACTIVE.load(Relaxed) {
        return false;
    }
    let p = ev.location();
    let s = desktop();
    let dx = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
    let dy = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
    let (l, t) = (s.x as f64, s.y as f64);
    let (r, b) = ((s.x + s.width) as f64 - 1.0, (s.y + s.height) as f64 - 1.0);
    let at_l = p.x <= l + 0.5 && dx <= 0;
    let at_r = p.x >= r - 0.5 && dx >= 0;
    let at_t = p.y <= t + 0.5 && dy <= 0;
    let at_b = p.y >= b - 0.5 && dy >= 0;
    let hit = if mask & EDGE_TL != 0 && at_l && at_t {
        EDGE_TL
    } else if mask & EDGE_TR != 0 && at_r && at_t {
        EDGE_TR
    } else if mask & EDGE_BL != 0 && at_l && at_b {
        EDGE_BL
    } else if mask & EDGE_BR != 0 && at_r && at_b {
        EDGE_BR
    } else if mask & EDGE_LEFT != 0 && at_l {
        EDGE_LEFT
    } else if mask & EDGE_RIGHT != 0 && at_r {
        EDGE_RIGHT
    } else if mask & EDGE_TOP != 0 && at_t {
        EDGE_TOP
    } else if mask & EDGE_BOTTOM != 0 && at_b {
        EDGE_BOTTOM
    } else {
        0
    };
    if hit == 0 {
        AT_BIT.store(0, Relaxed);
        AT_EDGE.store(0, Relaxed);
        EDGE_SINCE_MS.store(0, Relaxed);
        return false;
    }
    if AT_BIT.swap(hit, Relaxed) != hit {
        AT_EDGE.store(0, Relaxed);
        EDGE_SINCE_MS.store(0, Relaxed);
    }
    let now = now_ms().max(1);
    if EDGE_SINCE_MS.load(Relaxed) == 0 {
        EDGE_SINCE_MS.store(now, Relaxed);
    }
    if AT_EDGE.fetch_add(1, Relaxed) + 1 < PUSH_EVENTS {
        return false;
    }
    if now.saturating_sub(EDGE_SINCE_MS.load(Relaxed)) < SWITCH_DELAY_MS.load(Relaxed) as u64 {
        return false;
    }
    AT_BIT.store(0, Relaxed);
    AT_EDGE.store(0, Relaxed);
    EDGE_SINCE_MS.store(0, Relaxed);
    let fx = ((p.x - l) / (s.width as f64 - 1.0).max(1.0)) as f32;
    let fy = ((p.y - t) / (s.height as f64 - 1.0).max(1.0)) as f32;
    let (edge, frac) = match hit {
        EDGE_LEFT => (Edge::Left, fy),
        EDGE_RIGHT => (Edge::Right, fy),
        EDGE_TOP => (Edge::Top, fx),
        EDGE_TL => (Edge::TopLeft, 0.5),
        EDGE_TR => (Edge::TopRight, 0.5),
        EDGE_BL => (Edge::BottomLeft, 0.5),
        EDGE_BR => (Edge::BottomRight, 0.5),
        _ => (Edge::Bottom, fx),
    };
    let (cx, cy) = s.center();
    warp(cx as f64, cy as f64);
    set_capturing(true);
    emit(CaptureEvent::Crossed { edge, frac });
    true
}

/// The left button is down and the pointer moves: a window being dragged shows on the
/// other computer as it goes past an edge, and hangs across once the pointer gets there;
/// files being dragged go over with the pointer.
fn drag_edge(ev: &CGEvent, x: i32, y: i32) -> bool {
    let mask = EDGES.load(Relaxed);
    if mask == 0 || GAME_ACTIVE.load(Relaxed) {
        return false;
    }
    let s = desktop();
    let sides = side_edges();
    if SPANS.load(Relaxed) && !sides.is_empty() {
        crate::macspan::watch(&sides, (x, y), s, false);
    }
    let dx = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
    let dy = ev.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
    let edge = if x <= s.x && dx <= 0 {
        Edge::Left
    } else if x >= s.x + s.width - 1 && dx >= 0 {
        Edge::Right
    } else if y <= s.y && dy <= 0 {
        Edge::Top
    } else if y >= s.y + s.height - 1 && dy >= 0 {
        Edge::Bottom
    } else {
        return false;
    };
    if !sides.contains(&edge) {
        return false;
    }
    // A window held against the edge: it hangs across from here on.
    if SPANS.load(Relaxed) && crate::macspan::take(edge, (x, y), s, false) {
        return true;
    }
    // Files picked up in Finder (or on the desktop): over they go.
    if DRAG_FILES.load(Relaxed) {
        if let Some(paths) = dragged_files() {
            let frac = match edge {
                Edge::Left | Edge::Right => (y - s.y) as f32 / (s.height - 1).max(1) as f32,
                _ => (x - s.x) as f32 / (s.width - 1).max(1) as f32,
            };
            DRAG_COUNT.store(-1, Relaxed);
            // Esc ends Finder's drag without dropping anything here.
            post_key_pub(0x35);
            let (cx, cy) = s.center();
            warp(cx as f64, cy as f64);
            set_capturing(true);
            emit(CaptureEvent::DragCrossed { edge, frac, paths });
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- the platform

#[derive(Default)]
struct Pressed {
    keys: HashSet<u16>,
    buttons: HashSet<u8>,
    /// Modifier flags of the keys held down by the other computer.
    flags: u64,
}

pub struct MacPlatform {
    pressed: Mutex<Pressed>,
    /// For double clicks: when and where each button last went down, and how many in a row.
    clicks: Mutex<[(Option<Instant>, CGPoint, i64); 5]>,
}

impl MacPlatform {
    pub fn new() -> Self {
        MacPlatform {
            pressed: Mutex::new(Pressed::default()),
            clicks: Mutex::new([(None, CGPoint::new(0.0, 0.0), 0); 5]),
        }
    }

    fn post(&self, ev: &CGEvent) {
        ev.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, MAGIC);
        ev.post(CGEventTapLocation::HID);
    }

    fn post_button(&self, b: Button, down: bool) {
        let Some(src) = source() else { return };
        let at = cursor();
        let (kind, cg, idx, number) = match (b, down) {
            (Button::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left, 0, 0),
            (Button::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left, 0, 0),
            (Button::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right, 1, 1),
            (Button::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right, 1, 1),
            (Button::Middle, d) => (other(d), CGMouseButton::Center, 2, 2),
            (Button::X1, d) => (other(d), CGMouseButton::Center, 3, 3),
            (Button::X2, d) => (other(d), CGMouseButton::Center, 4, 4),
        };
        let Ok(ev) = CGEvent::new_mouse_event(src, kind, at, cg) else {
            return;
        };
        if b == Button::Left {
            if down {
                note_press(at.x, at.y);
            } else {
                crate::macspan::released();
            }
        }
        if number >= 2 {
            ev.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, number);
        }
        // Count clicks in a row, so double clicks work.
        let mut c = self.clicks.lock().unwrap();
        if down {
            let (last, pos, n) = c[idx];
            let near = (pos.x - at.x).abs() < 5.0 && (pos.y - at.y).abs() < 5.0;
            let quick = last.is_some_and(|l| l.elapsed() < Duration::from_millis(500));
            let count = if near && quick { n + 1 } else { 1 };
            c[idx] = (Some(Instant::now()), at, count);
        }
        ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, c[idx].2.max(1));
        drop(c);
        self.post(&ev);
        let mut p = self.pressed.lock().unwrap();
        if down {
            p.buttons.insert(idx as u8);
        } else {
            p.buttons.remove(&(idx as u8));
        }
    }

    fn post_key(&self, code: u16, down: bool) {
        let Some(src) = source() else { return };
        let Ok(ev) = CGEvent::new_keyboard_event(src, code, down) else {
            return;
        };
        let mut p = self.pressed.lock().unwrap();
        if let Some(flag) = mac_modifier_flag(code) {
            if code == 0x39 {
                // Caps Lock toggles on the way down only.
                if down {
                    p.flags ^= flag;
                }
            } else if down {
                p.flags |= flag;
            } else {
                p.flags &= !flag;
            }
            ev.set_type(CGEventType::FlagsChanged);
        }
        ev.set_flags(CGEventFlags::from_bits_truncate(p.flags));
        if down {
            p.keys.insert(code);
        } else {
            p.keys.remove(&code);
        }
        drop(p);
        self.post(&ev);
    }
}

impl Default for MacPlatform {
    fn default() -> Self {
        Self::new()
    }
}

fn other(down: bool) -> CGEventType {
    if down {
        CGEventType::OtherMouseDown
    } else {
        CGEventType::OtherMouseUp
    }
}

impl Platform for MacPlatform {
    fn screen(&self) -> Screen {
        desktop()
    }

    fn set_active_edges(&self, edges: &[Edge]) {
        EDGES.store(edge_mask(edges), Relaxed);
        AT_BIT.store(0, Relaxed);
        AT_EDGE.store(0, Relaxed);
    }

    fn begin_capture(&self) {
        let (cx, cy) = desktop().center();
        warp(cx as f64, cy as f64);
        AT_EDGE.store(0, Relaxed);
        set_capturing(true);
    }

    fn end_capture(&self, x: i32, y: i32) {
        set_capturing(false);
        AT_EDGE.store(0, Relaxed);
        warp(x as f64, y as f64);
    }

    fn inject(&self, ev: &InputEvent) {
        match ev {
            InputEvent::MouseMove { dx, dy } => {
                let s = desktop();
                let p = cursor();
                let x = (p.x + *dx as f64).clamp(s.x as f64, (s.x + s.width - 1) as f64);
                let y = (p.y + *dy as f64).clamp(s.y as f64, (s.y + s.height - 1) as f64);
                let held = self.pressed.lock().unwrap().buttons.clone();
                let (kind, button) = if held.contains(&0) {
                    (CGEventType::LeftMouseDragged, CGMouseButton::Left)
                } else if held.contains(&1) {
                    (CGEventType::RightMouseDragged, CGMouseButton::Right)
                } else if !held.is_empty() {
                    (CGEventType::OtherMouseDragged, CGMouseButton::Center)
                } else {
                    (CGEventType::MouseMoved, CGMouseButton::Left)
                };
                let Some(src) = source() else { return };
                if let Ok(e) = CGEvent::new_mouse_event(src, kind, CGPoint::new(x, y), button) {
                    e.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, *dx as i64);
                    e.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, *dy as i64);
                    self.post(&e);
                }
            }
            InputEvent::Button { button, down } => self.post_button(*button, *down),
            InputEvent::Wheel { dx, dy } => {
                let lines_y = *dy / 40; // 120 per notch on Windows, 3 lines per notch here
                let lines_x = -*dx / 40;
                if lines_x == 0 && lines_y == 0 {
                    return;
                }
                let Some(src) = source() else { return };
                if let Ok(e) =
                    CGEvent::new_scroll_event(src, ScrollEventUnit::LINE, 2, lines_y, lines_x, 0)
                {
                    self.post(&e);
                }
            }
            InputEvent::Key {
                vk,
                scan,
                extended,
                down,
            } => {
                if let Some(code) = win_to_mac(*vk, *scan, *extended) {
                    self.post_key(code, *down);
                }
            }
        }
    }

    fn cursor_pos(&self) -> (i32, i32) {
        let p = cursor();
        (p.x.round() as i32, p.y.round() as i32)
    }

    fn set_cursor(&self, x: i32, y: i32) {
        warp(x as f64, y as f64);
    }

    fn release_all(&self) {
        let (keys, buttons) = {
            let p = self.pressed.lock().unwrap();
            (p.keys.clone(), p.buttons.clone())
        };
        for k in keys {
            self.post_key(k, false);
        }
        for b in buttons {
            let button = match b {
                0 => Button::Left,
                1 => Button::Right,
                3 => Button::X1,
                4 => Button::X2,
                _ => Button::Middle,
            };
            self.post_button(button, false);
        }
        let mut p = self.pressed.lock().unwrap();
        p.keys.clear();
        p.buttons.clear();
        p.flags = 0;
    }

    fn clipboard_get(&self) -> Option<ClipContent> {
        clip::read()
    }

    fn clipboard_set(&self, c: &ClipContent) {
        clip::write(c);
    }

    fn set_switch_delay(&self, ms: u32) {
        SWITCH_DELAY_MS.store(ms, Relaxed);
    }

    fn drop_dir_at(&self, x: i32, y: i32) -> Option<PathBuf> {
        // Let go over the desktop: there. Anywhere else: Downloads.
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let on_desktop = crate::macwin::at(x as f64, y as f64).is_none();
        let dir = home.join(if on_desktop { "Desktop" } else { "Downloads" });
        dir.is_dir().then_some(dir)
    }

    fn set_game_mode(&self, on: bool) {
        GAME_MODE.store(on, Relaxed);
    }

    fn set_drag_files(&self, on: bool) {
        DRAG_FILES.store(on, Relaxed);
    }

    fn set_move_apps(&self, on: bool) {
        SPANS.store(on, Relaxed);
        if on {
            // Pictures of other programs' windows need Screen Recording.
            crate::macwin::ask_record();
        }
    }

    fn take_drag_files(&self) -> Option<Vec<PathBuf>> {
        let f = dragged_files();
        if f.is_some() {
            DRAG_COUNT.store(-1, Relaxed);
        }
        f
    }

    fn cancel_drag(&self) {
        post_key_pub(0x35);
    }

    fn launch_app(&self, app: &crate::proto::AppRef) -> Result<(), String> {
        crate::macapps::launch(app)
    }

    fn close_app(&self, app: &crate::proto::AppRef) {
        crate::macapps::close(app);
    }

    fn open_apps(&self) -> Vec<crate::proto::AppRef> {
        crate::macapps::open_apps()
    }

    fn edge_glow(&self, edge: Edge, frac: f32, rgb: [u8; 3]) {
        crate::macoverlay::glow(edge, frac, rgb);
    }

    fn span_show(&self, v: SpanView) {
        crate::macoverlay::note_place(&v);
        crate::macoverlay::show(v);
    }

    fn span_frame(&self, jpeg: Vec<u8>) {
        crate::macoverlay::frame(jpeg);
    }

    fn span_hide(&self, after_ms: u32) {
        crate::macoverlay::hide(after_ms);
    }

    fn span_restore(&self) {
        crate::macspan::restore();
    }

    fn span_watch(&self, edge: Edge) {
        crate::macspan::watch(&[edge], self.cursor_pos(), desktop(), true);
    }

    fn span_take(&self, edge: Edge) -> bool {
        let took = crate::macspan::take(edge, self.cursor_pos(), desktop(), true);
        if took {
            // macOS' drag was ended with a button-up of ours: the button isn't held here.
            self.pressed.lock().unwrap().buttons.remove(&0);
        }
        took
    }

    fn span_move(&self, dx: i32, dy: i32) {
        crate::macspan::mouse(dx, dy);
    }

    fn span_release(&self) {
        if crate::macspan::showing() {
            crate::macspan::release();
        }
    }

    fn span_cancel(&self) {
        crate::macspan::cancel();
    }

    fn trash(&self, paths: &[PathBuf]) -> bool {
        let Some(home) = std::env::var_os("HOME") else {
            return false;
        };
        let bin = PathBuf::from(home).join(".Trash");
        let mut ok = !paths.is_empty();
        for p in paths {
            let Some(name) = p.file_name() else { continue };
            // A name the Trash doesn't have yet.
            let mut target = bin.join(name);
            let mut n = 2;
            while target.exists() {
                target = bin.join(format!("{} {n}", name.to_string_lossy()));
                n += 1;
            }
            ok &= std::fs::rename(p, &target).is_ok();
        }
        ok
    }

    fn mirror_supported(&self) -> bool {
        true
    }

    fn mirror_adopt(&self, edge: Edge) -> Option<(u64, u32, u32)> {
        crate::macmirror::adopt(edge)
    }

    fn mirror_release(&self, id: u64, home: Option<f32>) {
        crate::macmirror::release(id, home);
    }

    fn portal_begin(&self, id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
        crate::macmirror::portal_begin(id, rx, ry)
    }

    fn portal_end(&self) {
        crate::macmirror::portal_end();
    }

    fn portal_outside(&self) -> Option<(i32, i32)> {
        crate::macmirror::portal_outside()
    }

    fn mirror_create(&self, id: u64, w: u32, h: u32, entry: Edge) -> bool {
        crate::macmirror::create(id, w, h, entry)
    }

    fn mirror_frame(&self, id: u64, w: u32, h: u32, jpeg: Vec<u8>) {
        crate::macmirror::frame(id, w, h, jpeg);
    }

    fn mirror_close(&self, id: u64) {
        crate::macmirror::close(id);
    }

    fn mirror_move(&self, id: u64, dx: i32, dy: i32, done: bool) -> Option<f32> {
        crate::macmirror::move_by(id, dx, dy, done)
    }

    fn mirror_at_cursor(&self) -> Option<(u64, i32, i32)> {
        crate::macmirror::at_cursor()
    }

    fn mirror_cursor(&self, id: u64, at: Option<(i32, i32)>) {
        crate::macmirror::cursor_on(id, at);
    }

    fn mirror_point(&self, id: u64, rx: i32, ry: i32) -> Option<(i32, i32)> {
        crate::macmirror::point(id, rx, ry)
    }

    fn pointer_hidden(&self, on: bool) {
        pointer_hidden(on);
    }

    fn device_info(&self) -> crate::proto::DeviceInfo {
        mac_device::info()
    }

    fn power(&self, a: crate::proto::PowerAction) {
        let arg = match a {
            crate::proto::PowerAction::Lock => "displaysleepnow",
            crate::proto::PowerAction::Sleep => "sleepnow",
        };
        let _ = std::process::Command::new("pmset").arg(arg).status();
    }

    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>) {
        if SINK.set(tx).is_ok() {
            let _ = std::thread::Builder::new()
                .name("tandem-tap".into())
                .spawn(tap_thread);
            let _ = std::thread::Builder::new()
                .name("tandem-clipwatch".into())
                .spawn(clip::watch);
            let _ = std::thread::Builder::new()
                .name("tandem-game".into())
                .spawn(game_watcher);
        }
    }
}

// ---------------------------------------------------------------- the clipboard

mod clip {
    use super::*;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL,
        NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardWriting,
    };
    use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSURL};

    /// macOS has no "clipboard changed" message: look at its change counter.
    pub fn watch() {
        let mut last = autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount());
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let now = autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount());
            if now != last {
                last = now;
                emit(CaptureEvent::ClipboardChanged);
            }
        }
    }

    fn bytes(d: &NSData) -> Vec<u8> {
        d.to_vec()
    }

    pub fn read() -> Option<ClipContent> {
        autoreleasepool(|_| unsafe {
            let pb = NSPasteboard::generalPasteboard();
            // Files copied in Finder.
            if let Some(items) = pb.pasteboardItems() {
                let mut files = Vec::new();
                for item in items.iter() {
                    if let Some(s) = item.stringForType(NSPasteboardTypeFileURL) {
                        if let Some(u) = NSURL::URLWithString(&s) {
                            if let Some(p) = u.path() {
                                files.push(PathBuf::from(p.to_string()));
                            }
                        }
                    }
                }
                if !files.is_empty() {
                    return Some(ClipContent::Files(files));
                }
            }
            let text = pb
                .stringForType(NSPasteboardTypeString)
                .map(|s| s.to_string());
            let png = pb.dataForType(NSPasteboardTypePNG).map(|d| bytes(&d));
            let tiff = pb.dataForType(NSPasteboardTypeTIFF);
            let has_picture = png.is_some() || tiff.is_some();
            // "Copy image" in a browser also puts the picture's link as text.
            let lone_link = |t: &str| {
                let t = t.trim();
                !t.contains(char::is_whitespace)
                    && (t.starts_with("http://") || t.starts_with("https://"))
            };
            if let Some(t) = text {
                if !(has_picture && (t.trim().is_empty() || lone_link(&t))) {
                    return Some(ClipContent::Text(t));
                }
            }
            if let Some(p) = png.and_then(|p| crate::image::trim_png(&p)) {
                return Some(ClipContent::Image(p));
            }
            // Screenshots and most apps put TIFF: turn it into PNG.
            let tiff = tiff?;
            let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
            let props = NSDictionary::new();
            let png = rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props)?;
            Some(ClipContent::Image(bytes(&png)))
        })
    }

    pub fn write(c: &ClipContent) {
        autoreleasepool(|_| unsafe {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            match c {
                ClipContent::Text(t) => {
                    pb.setString_forType(&NSString::from_str(t), NSPasteboardTypeString);
                }
                ClipContent::Image(png) => {
                    let data = NSData::with_bytes(png);
                    pb.setData_forType(Some(&data), NSPasteboardTypePNG);
                    // Older apps only paste TIFF.
                    if let Some(rep) = NSBitmapImageRep::imageRepWithData(&data) {
                        if let Some(tiff) = rep.TIFFRepresentation() {
                            pb.setData_forType(Some(&tiff), NSPasteboardTypeTIFF);
                        }
                    }
                }
                ClipContent::Files(paths) => {
                    let urls: Vec<Retained<NSURL>> = paths
                        .iter()
                        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
                        .collect();
                    let objs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = urls
                        .iter()
                        .map(|u| ProtocolObject::from_ref(&**u))
                        .collect();
                    let arr = NSArray::from_slice(&objs);
                    pb.writeObjects(&arr);
                }
            }
        })
    }
}

/// How the Mac is doing: battery and charging from `pmset`, read in the background every
/// 30 seconds (it is a separate program), and how long it has been on.
mod mac_device {
    use crate::proto::DeviceInfo;
    use std::sync::{Mutex, Once};
    use std::time::Duration;

    static CACHE: Mutex<Option<DeviceInfo>> = Mutex::new(None);
    static START: Once = Once::new();

    pub fn info() -> DeviceInfo {
        START.call_once(|| {
            let _ = std::thread::Builder::new().name("tandem-device".into()).spawn(|| loop {
                let mut d = std::process::Command::new("pmset")
                    .args(["-g", "batt"])
                    .output()
                    .map(|o| crate::status::parse_pmset(&String::from_utf8_lossy(&o.stdout)))
                    .unwrap_or_default();
                d.uptime_min = boot_minutes().unwrap_or(0);
                if let Ok(mut c) = CACHE.lock() {
                    *c = Some(d);
                }
                std::thread::sleep(Duration::from_secs(30));
            });
        });
        CACHE.lock().ok().and_then(|c| c.clone()).unwrap_or_default()
    }

    fn boot_minutes() -> Option<u32> {
        let out = std::process::Command::new("sysctl").args(["-n", "kern.boottime"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let sec: u64 = text.split("sec = ").nth(1)?.split(',').next()?.trim().parse().ok()?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some((now.saturating_sub(sec) / 60) as u32)
    }
}
