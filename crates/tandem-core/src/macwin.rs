//! Other programs' windows on the Mac: which one is under a point, where it is, a
//! picture of it, and moving it (live window drag, second-monitor mode, Liquid Glass).
//!
//! Positions are in the points macOS places the cursor in (top-left of the main display
//! is 0, 0). Pictures are in pixels (twice as many on a Retina display).
//!
//! Window lists and pictures come from the window server (CoreGraphics; pictures of
//! other programs need Screen Recording permission). Moving another program's window
//! goes through Accessibility, which CubeCursor already has for the keyboard and mouse.

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::display::CGDisplay;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window::{
    kCGWindowBounds, kCGWindowImageBestResolution, kCGWindowImageBoundsIgnoreFraming,
    kCGWindowLayer, kCGWindowListOptionIncludingWindow, kCGWindowListOptionOnScreenBelowWindow,
    kCGWindowListOptionOnScreenOnly, kCGWindowNumber, kCGWindowOwnerPID,
};
use std::ffi::c_void;

pub type WindowId = u32;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect) -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

type AxRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AxRef;
    fn AXUIElementCopyAttributeValue(el: AxRef, attr: CFStringRef, value: *mut CFTypeRef) -> i32;
    fn AXUIElementSetAttributeValue(el: AxRef, attr: CFStringRef, value: CFTypeRef) -> i32;
    fn AXUIElementPerformAction(el: AxRef, action: CFStringRef) -> i32;
    fn AXValueCreate(kind: u32, value: *const c_void) -> CFTypeRef;
    /// Private, but stable for years and used by every window manager on the Mac: the
    /// window server's number for an Accessibility window.
    fn _AXUIElementGetWindow(el: AxRef, id: *mut WindowId) -> i32;
}

const AX_POINT: u32 = 1;
const AX_SIZE: u32 = 2;

/// A rectangle in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl R {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Win {
    pub id: WindowId,
    pub pid: i32,
    pub layer: i64,
    pub bounds: R,
}

/// Shows macOS' prompt for Screen Recording (once).
pub fn ask_record() {
    unsafe {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
        }
    }
}

fn num(d: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<i64> {
    let k = unsafe { CFString::wrap_under_get_rule(key) };
    d.find(&k)?.downcast::<CFNumber>()?.to_i64()
}

fn bounds(d: &CFDictionary<CFString, CFType>) -> Option<R> {
    let k = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };
    let v = d.find(&k)?;
    let mut r = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(0.0, 0.0));
    let ok = unsafe {
        CGRectMakeWithDictionaryRepresentation(v.as_CFTypeRef() as CFDictionaryRef, &mut r)
    };
    ok.then_some(R {
        x: r.origin.x,
        y: r.origin.y,
        w: r.size.width,
        h: r.size.height,
    })
}

fn parse(arr: CFArray) -> Vec<Win> {
    let mut out = Vec::new();
    for p in arr.get_all_values() {
        if p.is_null() {
            continue;
        }
        let d: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_get_rule(p as CFDictionaryRef) };
        let (Some(id), Some(pid), Some(b)) = (
            num(&d, unsafe { kCGWindowNumber }),
            num(&d, unsafe { kCGWindowOwnerPID }),
            bounds(&d),
        ) else {
            continue;
        };
        out.push(Win {
            id: id as WindowId,
            pid: pid as i32,
            layer: num(&d, unsafe { kCGWindowLayer }).unwrap_or(0),
            bounds: b,
        });
    }
    out
}

/// The windows on screen, front to back.
pub fn on_screen() -> Vec<Win> {
    CGDisplay::window_list_info(kCGWindowListOptionOnScreenOnly, None)
        .map(parse)
        .unwrap_or_default()
}

/// One window, wherever it is (also off screen).
pub fn info(id: WindowId) -> Option<Win> {
    CGDisplay::window_list_info(kCGWindowListOptionIncludingWindow, Some(id))
        .map(parse)?
        .into_iter()
        .find(|w| w.id == id)
}

/// The topmost ordinary window (not a menu, the Dock or a panel of ours) under a point.
pub fn at(x: f64, y: f64) -> Option<Win> {
    let me = std::process::id() as i32;
    on_screen().into_iter().find(|w| {
        w.layer == 0
            && w.bounds.contains(x, y)
            && w.pid != me
            && w.bounds.w >= 40.0
            && w.bounds.h >= 30.0
    })
}

/// The topmost window of any program at a point (ours included), for "is our mirror on
/// top here?".
pub fn top_at(x: f64, y: f64) -> Option<Win> {
    on_screen()
        .into_iter()
        .find(|w| (0..=8).contains(&w.layer) && w.bounds.contains(x, y))
}

/// BGRA (premultiplied) pixels of a picture, and its size.
fn pixels(img: &CGImage) -> Option<(Vec<u8>, u32, u32)> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return None;
    }
    let space = CGColorSpace::create_device_rgb();
    let mut ctx = CGContext::create_bitmap_context(
        None,
        w,
        h,
        8,
        w * 4,
        &space,
        kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
    );
    ctx.draw_image(
        CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(w as f64, h as f64)),
        img,
    );
    Some((ctx.data().to_vec(), w as u32, h as u32))
}

/// A picture of one window (its whole content, also the parts off screen).
pub fn picture(id: WindowId) -> Option<(Vec<u8>, u32, u32)> {
    let null = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0.0, 0.0),
    );
    let img = CGDisplay::screenshot(
        null,
        kCGWindowListOptionIncludingWindow,
        id,
        kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution,
    )?;
    pixels(&img)
}

/// What is on screen in `r` underneath window `below` (that window and everything above
/// it left out): what Liquid Glass sees through. Nominal resolution: it gets blurred.
pub fn below(below: WindowId, r: R) -> Option<(Vec<u8>, u32, u32)> {
    let rect = CGRect::new(&CGPoint::new(r.x, r.y), &CGSize::new(r.w, r.h));
    let img = CGDisplay::screenshot(
        rect,
        kCGWindowListOptionOnScreenBelowWindow,
        below,
        core_graphics::window::kCGWindowImageNominalResolution,
    )?;
    pixels(&img)
}

/// Everything on screen in `r` (a screenshot).
pub fn screen(r: R) -> Option<(Vec<u8>, u32, u32)> {
    let rect = CGRect::new(&CGPoint::new(r.x, r.y), &CGSize::new(r.w, r.h));
    let img = CGDisplay::screenshot(
        rect,
        kCGWindowListOptionOnScreenOnly,
        0,
        core_graphics::window::kCGWindowImageNominalResolution,
    )?;
    pixels(&img)
}

// ------------------------------------------------------------------ Accessibility

/// An Accessibility element (released when dropped).
pub struct Ax(CFType);

// The element is a reference to another process' window: using it from any thread is
// what Accessibility is for.
unsafe impl Send for Ax {}
unsafe impl Sync for Ax {}

impl Ax {
    fn adopt(p: CFTypeRef) -> Option<Ax> {
        (!p.is_null()).then(|| Ax(unsafe { CFType::wrap_under_create_rule(p) }))
    }
    fn raw(&self) -> AxRef {
        self.0.as_CFTypeRef()
    }
    fn get(&self, attr: &str) -> Option<CFType> {
        let a = CFString::new(attr);
        let mut v: CFTypeRef = std::ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.raw(), a.as_concrete_TypeRef(), &mut v) };
        (err == 0 && !v.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(v) })
    }
    fn set(&self, attr: &str, v: CFTypeRef) -> bool {
        let a = CFString::new(attr);
        unsafe { AXUIElementSetAttributeValue(self.raw(), a.as_concrete_TypeRef(), v) == 0 }
    }
    pub fn flag(&self, attr: &str) -> bool {
        self.get(attr)
            .and_then(|v| v.downcast::<CFBoolean>())
            .map(bool::from)
            .unwrap_or(false)
    }
    pub fn set_flag(&self, attr: &str, on: bool) -> bool {
        let b = if on {
            CFBoolean::true_value()
        } else {
            CFBoolean::false_value()
        };
        self.set(attr, b.as_CFTypeRef())
    }
    pub fn move_to(&self, x: f64, y: f64) -> bool {
        let p = CGPoint::new(x, y);
        let Some(v) =
            Ax::adopt(unsafe { AXValueCreate(AX_POINT, &p as *const CGPoint as *const c_void) })
        else {
            return false;
        };
        self.set("AXPosition", v.raw())
    }
    pub fn resize(&self, w: f64, h: f64) -> bool {
        let s = CGSize::new(w, h);
        let Some(v) =
            Ax::adopt(unsafe { AXValueCreate(AX_SIZE, &s as *const CGSize as *const c_void) })
        else {
            return false;
        };
        self.set("AXSize", v.raw())
    }
    pub fn raise(&self) {
        let a = CFString::new("AXRaise");
        unsafe {
            AXUIElementPerformAction(self.raw(), a.as_concrete_TypeRef());
        }
    }
}

/// Brings program `pid` to the front (so a click lands on its window), the way window
/// managers do it.
pub fn bring_forward(pid: i32) {
    if let Some(app) = Ax::adopt(unsafe { AXUIElementCreateApplication(pid) }) {
        app.set_flag("AXFrontmost", true);
    }
}

/// The Accessibility window for window `id` of program `pid`.
pub fn ax_window(pid: i32, id: WindowId) -> Option<Ax> {
    let app = Ax::adopt(unsafe { AXUIElementCreateApplication(pid) })?;
    let list = app.get("AXWindows")?;
    let arr: CFArray = unsafe { CFArray::wrap_under_get_rule(list.as_CFTypeRef() as CFArrayRef) };
    for p in arr.get_all_values() {
        if p.is_null() {
            continue;
        }
        let mut wid: WindowId = 0;
        if unsafe { _AXUIElementGetWindow(p, &mut wid) } == 0 && wid == id {
            // `p` belongs to the array: take a reference of our own.
            let el = unsafe { CFType::wrap_under_get_rule(p) };
            return Some(Ax(el));
        }
    }
    None
}
