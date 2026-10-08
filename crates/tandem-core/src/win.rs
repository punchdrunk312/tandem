//! Windows implementation of `Platform`.
//!
//! * Capture: `WH_MOUSE_LL` / `WH_KEYBOARD_LL` hooks on a dedicated thread.
//!   While "capturing" they swallow physical input and forward it to the engine.
//! * Injection: `SendInput`.
//! * Clipboard: a message-only window with `AddClipboardFormatListener`.
//!
//! Hook callbacks must return quickly (Windows silently unhooks slow ones), so
//! they only touch atomics and push into an unbounded channel.

use crate::platform::*;
use crate::proto::{Button, Edge, InputEvent};
use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::UnboundedSender;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

static SINK: OnceLock<UnboundedSender<CaptureEvent>> = OnceLock::new();
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// 0 = none, 1 = left, 2 = right, 3 = top, 4 = bottom
static EDGE: AtomicU8 = AtomicU8::new(0);
/// Consecutive mouse-move events seen pushing against the active edge.
static AT_EDGE: AtomicU32 = AtomicU32::new(0);
/// How many such events it takes to cross. Stops a casual touch of the screen
/// edge (scrollbars, window borders) from stealing the mouse.
const PUSH_EVENTS: u32 = 4;

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

const HOTKEY_VK: u32 = 0x91; // Scroll Lock: jump to the other computer / emergency exit

fn emit(ev: CaptureEvent) {
    if let Some(s) = SINK.get() {
        let _ = s.send(ev);
    }
}

fn edge_code(e: Option<Edge>) -> u8 {
    match e {
        None => 0,
        Some(Edge::Left) => 1,
        Some(Edge::Right) => 2,
        Some(Edge::Top) => 3,
        Some(Edge::Bottom) => 4,
    }
}

fn vscreen() -> Screen {
    unsafe {
        let s = Screen {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        };
        if s.width <= 0 || s.height <= 0 {
            Screen {
                x: 0,
                y: 0,
                width: GetSystemMetrics(SM_CXSCREEN).max(1),
                height: GetSystemMetrics(SM_CYSCREEN).max(1),
            }
        } else {
            s
        }
    }
}

// ------------------------------------------------------------------ hooks

#[repr(C)]
struct MsLl {
    x: i32,
    y: i32,
    data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
struct KbdLl {
    vk: u32,
    scan: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

const LLMHF_INJECTED: u32 = 0x01;
const LLMHF_LOWER_IL_INJECTED: u32 = 0x02;
const LLKHF_EXTENDED: u32 = 0x01;
const LLKHF_LOWER_IL_INJECTED: u32 = 0x02;
const LLKHF_INJECTED: u32 = 0x10;

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let h = &*(lp.0 as *const MsLl);
        let swallow =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mouse_inner(wp.0 as u32, h)))
                .unwrap_or(false);
        if swallow {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn mouse_inner(msg: u32, h: &MsLl) -> bool {
    if h.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) != 0 {
        return false; // our own SendInput, or another tool's
    }
    let s = vscreen();
    if !CAPTURING.load(Relaxed) {
        let edge = EDGE.load(Relaxed);
        if edge == 0 || msg != WM_MOUSEMOVE {
            return false;
        }
        let at_edge = match edge {
            1 => h.x <= s.x,
            2 => h.x >= s.x + s.width - 1,
            3 => h.y <= s.y,
            _ => h.y >= s.y + s.height - 1,
        };
        // Dragging a window or selection along the edge must not switch computers.
        let button_down = unsafe {
            GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 || GetAsyncKeyState(VK_RBUTTON.0 as i32) < 0
        };
        if !at_edge || button_down {
            AT_EDGE.store(0, Relaxed);
            return false;
        }
        if AT_EDGE.fetch_add(1, Relaxed) + 1 < PUSH_EVENTS {
            return false;
        }
        AT_EDGE.store(0, Relaxed);
        let fx = (h.x - s.x) as f32 / (s.width - 1).max(1) as f32;
        let fy = (h.y - s.y) as f32 / (s.height - 1).max(1) as f32;
        let (edge, frac) = match edge {
            1 => (Edge::Left, fy),
            2 => (Edge::Right, fy),
            3 => (Edge::Top, fx),
            _ => (Edge::Bottom, fx),
        };
        CAPTURING.store(true, Relaxed);
        let (cx, cy) = s.center();
        unsafe {
            let _ = SetCursorPos(cx, cy);
        }
        emit(CaptureEvent::Crossed { edge, frac });
        return true;
    }

    // Capturing: everything physical goes to the other computer.
    let ev = match msg {
        WM_MOUSEMOVE => {
            let (cx, cy) = s.center();
            let (dx, dy) = (h.x - cx, h.y - cy);
            // Park the cursor again so we always measure from the same point.
            unsafe {
                let _ = SetCursorPos(cx, cy);
            }
            if dx == 0 && dy == 0 {
                return true;
            }
            Some(InputEvent::MouseMove { dx, dy })
        }
        WM_LBUTTONDOWN => Some(InputEvent::Button {
            button: Button::Left,
            down: true,
        }),
        WM_LBUTTONUP => Some(InputEvent::Button {
            button: Button::Left,
            down: false,
        }),
        WM_RBUTTONDOWN => Some(InputEvent::Button {
            button: Button::Right,
            down: true,
        }),
        WM_RBUTTONUP => Some(InputEvent::Button {
            button: Button::Right,
            down: false,
        }),
        WM_MBUTTONDOWN => Some(InputEvent::Button {
            button: Button::Middle,
            down: true,
        }),
        WM_MBUTTONUP => Some(InputEvent::Button {
            button: Button::Middle,
            down: false,
        }),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let b = if (h.data >> 16) & 0xffff == 2 {
                Button::X2
            } else {
                Button::X1
            };
            Some(InputEvent::Button {
                button: b,
                down: msg == WM_XBUTTONDOWN,
            })
        }
        WM_MOUSEWHEEL => Some(InputEvent::Wheel {
            dx: 0,
            dy: ((h.data >> 16) as u16 as i16) as i32,
        }),
        WM_MOUSEHWHEEL => Some(InputEvent::Wheel {
            dx: ((h.data >> 16) as u16 as i16) as i32,
            dy: 0,
        }),
        _ => None,
    };
    if let Some(ev) = ev {
        emit(CaptureEvent::Input(ev));
    }
    true
}

unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(lp.0 as *const KbdLl);
        let swallow =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| key_inner(wp.0 as u32, k)))
                .unwrap_or(false);
        if swallow {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn key_inner(msg: u32, k: &KbdLl) -> bool {
    if k.flags & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED) != 0 {
        return false;
    }
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    if k.vk == HOTKEY_VK {
        if down {
            if CAPTURING.swap(false, Relaxed) {
                // Emergency exit: give the mouse back *here*, synchronously.
                let (cx, cy) = vscreen().center();
                unsafe {
                    let _ = SetCursorPos(cx, cy);
                }
                emit(CaptureEvent::Failsafe);
            } else if EDGE.load(Relaxed) != 0 {
                emit(CaptureEvent::HotkeyEngage);
            }
        }
        return true;
    }
    if !CAPTURING.load(Relaxed) {
        return false;
    }
    emit(CaptureEvent::Input(InputEvent::Key {
        vk: k.vk as u16,
        scan: k.scan as u16,
        extended: k.flags & LLKHF_EXTENDED != 0,
        down,
    }));
    true
}

unsafe extern "system" fn sink_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        emit(CaptureEvent::ClipboardChanged);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn hook_thread() {
    unsafe {
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => HINSTANCE(h.0),
            Err(e) => {
                log::error!("GetModuleHandleW: {e}");
                return;
            }
        };
        if let Err(e) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst), 0) {
            log::error!("mouse hook: {e}");
        }
        if let Err(e) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(hinst), 0) {
            log::error!("keyboard hook: {e}");
        }

        let wc = WNDCLASSW {
            lpfnWndProc: Some(sink_wndproc),
            hInstance: hinst,
            lpszClassName: w!("TandemSink"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("TandemSink"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        ) {
            Ok(hwnd) => {
                if let Err(e) = AddClipboardFormatListener(hwnd) {
                    log::error!("clipboard listener: {e}");
                }
            }
            Err(e) => log::error!("sink window: {e}"),
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ------------------------------------------------------------------ platform

#[derive(Default)]
struct Pressed {
    keys: HashSet<(u16, u16, bool)>,
    buttons: HashSet<u8>,
}

pub struct WinPlatform {
    pressed: Mutex<Pressed>,
}

impl WinPlatform {
    pub fn new() -> Self {
        // Coordinates must be physical pixels on every monitor.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        Self {
            pressed: Mutex::new(Pressed::default()),
        }
    }
}

impl Default for WinPlatform {
    fn default() -> Self {
        Self::new()
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(vk: u16, scan: u16, extended: bool, down: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn button_flags(b: Button, down: bool) -> (MOUSE_EVENT_FLAGS, u32) {
    match (b, down) {
        (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (Button::X1, d) => (
            if d {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            1,
        ),
        (Button::X2, d) => (
            if d {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            2,
        ),
    }
}

fn button_id(b: Button) -> u8 {
    match b {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::X1 => 3,
        Button::X2 => 4,
    }
}

fn id_button(i: u8) -> Button {
    match i {
        0 => Button::Left,
        1 => Button::Right,
        2 => Button::Middle,
        3 => Button::X1,
        _ => Button::X2,
    }
}

impl Platform for WinPlatform {
    fn screen(&self) -> Screen {
        vscreen()
    }

    fn set_active_edge(&self, edge: Option<Edge>) {
        EDGE.store(edge_code(edge), Relaxed);
        AT_EDGE.store(0, Relaxed);
    }

    fn begin_capture(&self) {
        let (cx, cy) = vscreen().center();
        unsafe {
            let _ = SetCursorPos(cx, cy);
        }
        AT_EDGE.store(0, Relaxed);
        CAPTURING.store(true, Relaxed);
    }

    fn end_capture(&self, x: i32, y: i32) {
        CAPTURING.store(false, Relaxed);
        AT_EDGE.store(0, Relaxed);
        unsafe {
            let _ = SetCursorPos(x, y);
        }
    }

    fn inject(&self, ev: &InputEvent) {
        match ev {
            InputEvent::MouseMove { dx, dy } => {
                // Absolute positioning: the sender's deltas are already
                // accelerated, so don't let this machine accelerate them again.
                let s = vscreen();
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                }
                let nx = (p.x + dx).clamp(s.x, s.x + s.width - 1);
                let ny = (p.y + dy).clamp(s.y, s.y + s.height - 1);
                let ax = ((nx - s.x) as i64 * 65535 / (s.width - 1).max(1) as i64) as i32;
                let ay = ((ny - s.y) as i64 * 65535 / (s.height - 1).max(1) as i64) as i32;
                send(&[mouse_input(
                    ax,
                    ay,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )]);
            }
            InputEvent::Button { button, down } => {
                let (f, data) = button_flags(*button, *down);
                send(&[mouse_input(0, 0, data, f)]);
                let mut p = self.pressed.lock().unwrap();
                if *down {
                    p.buttons.insert(button_id(*button));
                } else {
                    p.buttons.remove(&button_id(*button));
                }
            }
            InputEvent::Wheel { dx, dy } => {
                if *dy != 0 {
                    send(&[mouse_input(0, 0, *dy as u32, MOUSEEVENTF_WHEEL)]);
                }
                if *dx != 0 {
                    send(&[mouse_input(0, 0, *dx as u32, MOUSEEVENTF_HWHEEL)]);
                }
            }
            InputEvent::Key {
                vk,
                scan,
                extended,
                down,
            } => {
                send(&[key_input(*vk, *scan, *extended, *down)]);
                let mut p = self.pressed.lock().unwrap();
                if *down {
                    p.keys.insert((*vk, *scan, *extended));
                } else {
                    p.keys.retain(|(v, _, _)| v != vk);
                }
            }
        }
    }

    fn cursor_pos(&self) -> (i32, i32) {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        (p.x, p.y)
    }

    fn set_cursor(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetCursorPos(x, y);
        }
    }

    fn release_all(&self) {
        let mut p = self.pressed.lock().unwrap();
        for (vk, scan, ext) in p.keys.drain() {
            send(&[key_input(vk, scan, ext, false)]);
        }
        for b in p.buttons.drain() {
            let (f, data) = button_flags(id_button(b), false);
            send(&[mouse_input(0, 0, data, f)]);
        }
    }

    fn clipboard_get(&self) -> Option<ClipContent> {
        unsafe {
            if !open_clipboard() {
                return None;
            }
            let r = read_clipboard();
            let _ = CloseClipboard();
            r
        }
    }

    fn clipboard_set(&self, c: &ClipContent) {
        unsafe {
            if !open_clipboard() {
                return;
            }
            let _ = EmptyClipboard();
            match c {
                ClipContent::Text(t) => {
                    let mut bytes: Vec<u8> = t
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .flat_map(|u| u.to_le_bytes())
                        .collect();
                    bytes.shrink_to_fit();
                    put(CF_UNICODETEXT, &bytes);
                }
                ClipContent::Files(paths) => {
                    // DROPFILES header: pFiles=20, pt=(0,0), fNC=0, fWide=1
                    let mut bytes: Vec<u8> = Vec::new();
                    for v in [20u32, 0, 0, 0, 1] {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    for p in paths {
                        for u in p.as_os_str().encode_wide().chain(std::iter::once(0)) {
                            bytes.extend_from_slice(&u.to_le_bytes());
                        }
                    }
                    bytes.extend_from_slice(&[0, 0]); // list terminator
                    put(CF_HDROP, &bytes);
                    // Paste should copy, never move, our temporary files.
                    let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
                    if fmt != 0 {
                        put(fmt, &1u32.to_le_bytes());
                    }
                }
            }
            let _ = CloseClipboard();
        }
    }

    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>) {
        if SINK.set(tx).is_ok() {
            if let Err(e) = std::thread::Builder::new()
                .name("tandem-input".into())
                .spawn(hook_thread)
            {
                log::error!("cannot start input thread: {e}");
            }
        }
    }
}

unsafe fn open_clipboard() -> bool {
    for _ in 0..20 {
        if OpenClipboard(None).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// Hand `bytes` to the clipboard in `format`. The system owns the memory on success.
unsafe fn put(format: u32, bytes: &[u8]) {
    let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else {
        return;
    };
    let p = GlobalLock(h);
    if p.is_null() {
        return;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    let _ = GlobalUnlock(h);
    let _ = SetClipboardData(format, Some(HANDLE(h.0)));
}

unsafe fn read_clipboard() -> Option<ClipContent> {
    // Password managers mark secrets with this format: never sync those.
    let excl = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    if excl != 0 && IsClipboardFormatAvailable(excl).is_ok() {
        return None;
    }
    if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
        let h = GetClipboardData(CF_HDROP).ok()?;
        let drop = HDROP(h.0);
        let n = DragQueryFileW(drop, 0xFFFF_FFFF, None);
        let mut out = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(
                &buf[..got.min(len)],
            )));
        }
        return if out.is_empty() {
            None
        } else {
            Some(ClipContent::Files(out))
        };
    }
    if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
        let h = GetClipboardData(CF_UNICODETEXT).ok()?;
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u16;
        if p.is_null() {
            return None;
        }
        let max = GlobalSize(g) / 2;
        let mut len = 0usize;
        while len < max && *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        let _ = GlobalUnlock(g);
        return Some(ClipContent::Text(s));
    }
    None
}
