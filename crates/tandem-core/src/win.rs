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
use crate::proto::{AppRef, Button, Edge, InputEvent};
use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::UnboundedSender;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Media::timeBeginPeriod;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::StationsAndDesktops::*;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragQueryFileW, ShellExecuteW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

static SINK: OnceLock<UnboundedSender<CaptureEvent>> = OnceLock::new();
static CAPTURING: AtomicBool = AtomicBool::new(false);

/// Flips the capture flag and, when it really changed, tells the cursor thread to hide
/// or show the pointer. Returns the previous value.
fn set_capturing(on: bool) -> bool {
    let was = CAPTURING.swap(on, Relaxed);
    if was != on {
        if let Some(tx) = CURSOR_TX.get() {
            let _ = tx.send(on);
        }
    }
    was
}
/// Edges that lead to a connected computer, one bit each (see `EDGE_*`). 0 = none.
static EDGES: AtomicU8 = AtomicU8::new(0);
/// The edge bit the cursor is currently pushing against (0 = none).
static AT_BIT: AtomicU8 = AtomicU8::new(0);
const EDGE_LEFT: u8 = 1;
const EDGE_RIGHT: u8 = 2;
const EDGE_TOP: u8 = 4;
const EDGE_BOTTOM: u8 = 8;
const EDGE_TL: u8 = 16;
const EDGE_TR: u8 = 32;
const EDGE_BL: u8 = 64;
const EDGE_BR: u8 = 128;
/// Consecutive mouse-move events seen pushing against the active edge.
static AT_EDGE: AtomicU32 = AtomicU32::new(0);
/// How many such events it takes to cross. Stops a casual touch of the screen
/// edge (scrollbars, window borders) from stealing the mouse.
const PUSH_EVENTS: u32 = 4;

// ------------------------------------------------------- dragging files across
//
// Windows has no API for "what is being dragged right now", so the drag is read from
// the shell instead: when the left button goes down on an item in an Explorer window or
// on the desktop, a helper asks Explorer (through a small resident PowerShell process
// and its Shell.Application object) which items are selected. If the cursor then runs
// into a connected edge with the button still held, the files are handed to the engine
// and the local drag is cancelled. Anything that goes wrong here only means the drag
// does not cross; it never affects normal mouse sharing.

static DRAG_FILES: AtomicBool = AtomicBool::new(true);
/// Files are in hand (see `Platform::set_carrying`): the button may stay down at an edge.
static CARRYING: AtomicBool = AtomicBool::new(false);
/// Bumped on every left-button press and release, so a stale answer is never used.
static DRAG_GEN: AtomicU32 = AtomicU32::new(1);
/// The press `DRAG_PATHS` belongs to.
static DRAG_READY_GEN: AtomicU32 = AtomicU32::new(0);
static DRAG_PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static DRAG_TX: OnceLock<std::sync::mpsc::Sender<DragReq>> = OnceLock::new();

// ------------------------------------------------------ moving a program across
//
// Dragging a program's window (or a browser tab) against an edge that leads to another
// computer and holding it there for a moment offers to move the program: the other
// computer opens its own copy and, for chat and music programs, this one closes. Which
// program is under the press is worked out the instant the button goes down, off the
// hook thread; a browser's page is read from its address bar while the drag is still on
// its way. Anything that goes wrong just means nothing is offered.

static MOVE_APPS: AtomicBool = AtomicBool::new(false);
/// The press the program under it was worked out for.
static APP_GEN: AtomicU32 = AtomicU32::new(0);
/// 0 = nothing to move, 1 = a browser's page is still being read, 2 = ready.
static APP_STATE: AtomicU8 = AtomicU8::new(0);
static APP_DRAG: Mutex<Option<AppRef>> = Mutex::new(None);
/// How long a dragged window must be held against the edge before it is offered, so
/// snapping a window to the side of the screen never triggers it by accident.
const APP_DWELL_US: u64 = 500_000;

#[derive(Clone, Copy)]
enum ShellKind {
    Desktop,
    Explorer(isize),
}

impl ShellKind {
    fn arg(self) -> String {
        match self {
            ShellKind::Desktop => "D 0".into(),
            ShellKind::Explorer(h) => format!("E {h}"),
        }
    }
}

enum DragReq {
    Warm,
    Stop,
    Sel {
        seq: u32,
        x: i32,
        y: i32,
    },
    App {
        seq: u32,
        x: i32,
        y: i32,
    },
    Dir {
        kind: ShellKind,
        reply: std::sync::mpsc::Sender<Option<String>>,
    },
}

const PS_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$sh = New-Object -ComObject Shell.Application
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
function Docs($k, $h) {
  if ($k -eq 'D') {
    $x = 0
    $r = $sh.Windows().FindWindowSW([ref]0, [ref]0, 8, [ref]$x, 1)
    if ($r) { return @($r.Document) }
    return @()
  }
  $o = @()
  foreach ($w in $sh.Windows()) { if ($w.HWND -eq $h) { $o += $w.Document } }
  return $o
}
while ($true) {
  $l = [Console]::In.ReadLine()
  if ($l -eq $null) { break }
  $p = $l.Split(' ')
  $out = 'NONE'
  try {
    if ($p[0] -eq 'URL') {
      $el = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$p[1])
      $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
      $first = $el.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
      $cands = @()
      if ($first) { $cands += $first }
      foreach ($e in $cands) {
        $v = $e.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
        if ($v -and [string]$e.Current.Name -match 'address|url|search|location') { $out = 'OK|' + $v }
      }
      if ($out -eq 'NONE') {
        foreach ($e in $el.FindAll([System.Windows.Automation.TreeScope]::Descendants, $c)) {
          if ([string]$e.Current.Name -match 'address|url|search|location') {
            $v = $e.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
            if ($v) { $out = 'OK|' + $v; break }
          }
        }
      }
    } else {
    $docs = Docs $p[1] ([int64]$p[2])
    if ($p[0] -eq 'SEL') {
      foreach ($d in $docs) {
        $o = @()
        foreach ($i in $d.SelectedItems()) { $o += $i.Path }
        if ($o.Count -gt 0) { $out = 'OK|' + ($o -join '|'); break }
      }
    } elseif ($p[0] -eq 'DIR') {
      if ($p[1] -eq 'D') { $out = 'OK|' + [Environment]::GetFolderPath('Desktop') }
      elseif ($docs.Count -gt 0) { $out = 'OK|' + $docs[0].Folder.Self.Path }
    }
    }
  } catch { }
  [Console]::Out.WriteLine($out)
  [Console]::Out.Flush()
}
"#;

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

/// The resident PowerShell that answers shell questions, one line in, one line out.
struct Ps {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
}

impl Ps {
    fn start() -> Option<Ps> {
        use std::io::BufRead;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        let utf16: Vec<u8> = PS_SCRIPT
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NoLogo",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &base64(&utf16),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let (tx, lines) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("tandem-ps-read".into())
            .spawn(move || {
                for line in std::io::BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        Some(Ps {
            child,
            stdin,
            lines,
        })
    }

    fn ask(&mut self, line: &str, wait: std::time::Duration) -> Option<String> {
        use std::io::Write;
        while self.lines.try_recv().is_ok() {} // anything left over is an old answer
        writeln!(self.stdin, "{line}").ok()?;
        self.stdin.flush().ok()?;
        let r = self.lines.recv_timeout(wait).ok()?;
        Some(r.trim_start_matches('\u{feff}').trim().to_string())
    }
}

impl Drop for Ps {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_paths(reply: Option<String>) -> Vec<PathBuf> {
    reply
        .as_deref()
        .and_then(|r| r.strip_prefix("OK|"))
        .map(|r| {
            r.split('|')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default()
}

fn ask_ps(ps: &mut Option<Ps>, line: &str) -> Option<String> {
    ask_ps_for(ps, line, 1500)
}

fn ask_ps_for(ps: &mut Option<Ps>, line: &str, wait_ms: u64) -> Option<String> {
    if ps.is_none() {
        *ps = Ps::start();
    }
    let r = ps
        .as_mut()?
        .ask(line, std::time::Duration::from_millis(wait_ms));
    if r.is_none() {
        *ps = None; // dead or stuck: start a fresh one next time
    }
    r
}

fn drag_thread(rx: std::sync::mpsc::Receiver<DragReq>) {
    let mut ps: Option<Ps> = None;
    for req in rx {
        match req {
            DragReq::Stop => ps = None,
            DragReq::Warm => {
                if ps.is_none() {
                    ps = Ps::start();
                }
            }
            DragReq::Sel { seq, x, y } => {
                // Explorer changes the selection on press; give it a moment.
                std::thread::sleep(std::time::Duration::from_millis(80));
                let held = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 };
                if !held || DRAG_GEN.load(Relaxed) != seq {
                    continue;
                }
                // Looked up here, not in the mouse hook, which must never wait on windows.
                let Some(kind) = shell_kind_at(x, y, true) else {
                    continue;
                };
                let paths = parse_paths(ask_ps(&mut ps, &format!("SEL {}", kind.arg())));
                log::info!(
                    "file drag: {} selected item(s) under the press",
                    paths.len()
                );
                if paths.is_empty() || DRAG_GEN.load(Relaxed) != seq {
                    continue;
                }
                if let Ok(mut p) = DRAG_PATHS.lock() {
                    *p = paths;
                    DRAG_READY_GEN.store(seq, Relaxed);
                }
            }
            DragReq::App { seq, x, y } => {
                if DRAG_GEN.load(Relaxed) != seq {
                    continue;
                }
                let Some((hwnd, mut app)) = window_app_at(x, y) else {
                    continue;
                };
                let still = || DRAG_GEN.load(Relaxed) == seq;
                if app.is_browser() {
                    // Wait for the page to be read before the edge may offer anything.
                    APP_GEN.store(seq, Relaxed);
                    APP_STATE.store(1, Relaxed);
                    let mut url = None;
                    for _ in 0..2 {
                        url = parse_url(ask_ps_for(&mut ps, &format!("URL {hwnd}"), 4000));
                        if url.is_some() || !still() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    if url.is_none() {
                        log::info!("app drag: could not read the page address of {}", app.exe);
                        if APP_GEN.load(Relaxed) == seq {
                            APP_STATE.store(0, Relaxed);
                        }
                        continue;
                    }
                    app.url = url;
                }
                if !app.is_sane() || !still() {
                    if APP_GEN.load(Relaxed) == seq {
                        APP_STATE.store(0, Relaxed);
                    }
                    continue;
                }
                log::info!("app drag: {} ready", app.exe);
                if let Ok(mut a) = APP_DRAG.lock() {
                    *a = Some(app);
                    APP_GEN.store(seq, Relaxed);
                    APP_STATE.store(2, Relaxed);
                }
            }
            DragReq::Dir { kind, reply } => {
                let r = ask_ps(&mut ps, &format!("DIR {}", kind.arg()));
                let _ = reply.send(r);
            }
        }
    }
}

/// What kind of shell surface is at this point, if it is the *items* area of one (so a
/// press on a title bar or a scrollbar is never mistaken for dragging files).
fn shell_kind_at(x: i32, y: i32, items_only: bool) -> Option<ShellKind> {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return None;
        }
        let class_of = |h: HWND| {
            let mut buf = [0u16; 64];
            let n = GetClassNameW(h, &mut buf);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        };
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.0.is_null() {
            return None;
        }
        if items_only {
            let c = class_of(hwnd);
            if c != "SysListView32" && c != "DirectUIHWND" {
                return None;
            }
        }
        match class_of(root).as_str() {
            "CabinetWClass" => Some(ShellKind::Explorer(root.0 as isize)),
            "Progman" | "WorkerW" => Some(ShellKind::Desktop),
            _ => None,
        }
    }
}

fn ensure_drag_thread() {
    if DRAG_TX.get().is_none() {
        let (tx, rx) = std::sync::mpsc::channel();
        if DRAG_TX.set(tx).is_ok()
            && std::thread::Builder::new()
                .name("tandem-drag".into())
                .spawn(move || drag_thread(rx))
                .is_err()
        {
            log::error!("cannot start the drag helper");
        }
    }
}

/// Opens a file, a shortcut or a web address the way double-clicking it would.
fn shell_open(target: &str) -> Result<(), ()> {
    shell_open_with(target, None)
}

fn shell_open_with(target: &str, params: Option<&str>) -> Result<(), ()> {
    unsafe {
        let params = params.map(HSTRING::from);
        let r = ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            match &params {
                Some(p) => PCWSTR(p.as_ptr()),
                None => PCWSTR::null(),
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if r.0 as isize > 32 {
            Ok(())
        } else {
            Err(())
        }
    }
}

/// How to start a program: the file to open and, for a few, the arguments.
struct Launch {
    file: String,
    params: Option<String>,
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

/// Where to start `app` from on this computer. Only ever looks for the program by its
/// file name (already checked to be plain), in the places programs are installed:
/// Start menu shortcuts, the usual install folders (including the self-updating
/// per-user kind Discord uses), Windows' list of installed apps (which also has Store
/// apps), and programs Windows has registered by name.
fn find_program(app: &AppRef) -> Option<Launch> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let exe = app.exe.clone();
    let stem = exe[..exe.len() - 4].to_string();
    let lower = stem.to_ascii_lowercase();

    // 1. A Start menu shortcut named like the program ("Discord.lnk", "Discord Inc/Discord.lnk").
    let mut menus: Vec<PathBuf> = Vec::new();
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = env_dir(var) {
            menus.push(base.join(r"Microsoft\Windows\Start Menu\Programs"));
        }
    }
    let mut budget = 8000usize;
    let mut loose: Option<String> = None;
    let mut stack: Vec<(PathBuf, u8)> = menus.iter().cloned().map(|m| (m, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let path = e.path();
            if path.is_dir() {
                if depth < 3 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("lnk"))
            {
                continue;
            }
            let name = path
                .file_stem()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if name == lower {
                return Some(Launch {
                    file: path.to_string_lossy().into_owned(),
                    params: None,
                });
            }
            if loose.is_none() && name.starts_with(&lower) && !name.contains("uninstall") {
                loose = Some(path.to_string_lossy().into_owned());
            }
        }
    }
    if let Some(file) = loose {
        return Some(Launch { file, params: None });
    }

    // 2. The usual install folders. Squirrel apps (Discord, older Slack and Teams) live in
    //    %LOCALAPPDATA%\<Name> and are started through their Update.exe.
    let local = env_dir("LOCALAPPDATA");
    if let Some(local) = &local {
        let update = local.join(&stem).join("Update.exe");
        if update.is_file() {
            return Some(Launch {
                file: update.to_string_lossy().into_owned(),
                params: Some(format!("--processStart {exe}")),
            });
        }
    }
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(l) = &local {
        bases.push(l.clone());
        bases.push(l.join("Programs"));
    }
    for var in ["APPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
        bases.extend(env_dir(var));
    }
    for base in bases {
        let direct = base.join(&stem).join(&exe);
        if direct.is_file() {
            return Some(Launch {
                file: direct.to_string_lossy().into_owned(),
                params: None,
            });
        }
    }

    // 3. One PowerShell look: a shortcut that points at the program, else Windows' list of
    //    installed apps (classic and Store) by name.
    let script = format!(
        "$w = New-Object -ComObject WScript.Shell; \
         $d = @($env:ProgramData, $env:APPDATA) | % {{ Join-Path $_ 'Microsoft\\Windows\\Start Menu\\Programs' }}; \
         foreach ($f in Get-ChildItem $d -Recurse -Filter *.lnk -ErrorAction SilentlyContinue) {{ \
           $t = $w.CreateShortcut($f.FullName).TargetPath; \
           if ($t -and ([IO.Path]::GetFileName($t) -ieq '{exe}')) {{ 'LNK|' + $f.FullName; exit }} }}; \
         $a = Get-StartApps | ? {{ $_.Name -like '*{stem}*' }} | select -First 1; \
         if ($a) {{ 'APP|' + $a.AppID }}"
    );
    if let Ok(mut child) = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
    {
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < std::time::Duration::from_secs(15) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                _ => {
                    let _ = child.kill();
                    break;
                }
            }
        }
        if let Some(mut out) = child.stdout.take() {
            use std::io::Read;
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            let line = text.lines().next().unwrap_or("").trim().to_string();
            if let Some(p) = line.strip_prefix("LNK|") {
                if std::path::Path::new(p).exists() {
                    return Some(Launch {
                        file: p.to_string(),
                        params: None,
                    });
                }
            } else if let Some(id) = line.strip_prefix("APP|") {
                let ok = !id.is_empty()
                    && id.chars().all(|c| {
                        c.is_ascii_alphanumeric()
                            || matches!(
                                c,
                                '.' | '_' | '!' | '{' | '}' | '-' | '\\' | ':' | ' ' | '(' | ')'
                            )
                    });
                if ok {
                    return Some(Launch {
                        file: format!(r"shell:AppsFolder\{id}"),
                        params: None,
                    });
                }
            }
        }
    }

    // 4. Registered with Windows by name ("App Paths"): the bare file name starts it.
    for root in ["HKCU", "HKLM"] {
        let key = format!(r"{root}\Software\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
        let found = Command::new("reg")
            .args(["query", &key, "/ve"])
            .creation_flags(0x0800_0000)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if found {
            return Some(Launch {
                file: exe,
                params: None,
            });
        }
    }
    log::info!("app move: no way found to start {}", app.exe);
    None
}

/// The address a browser reported, made into a full web address (or nothing).
fn parse_url(reply: Option<String>) -> Option<String> {
    let v = reply?.strip_prefix("OK|")?.trim().to_string();
    if v.is_empty() || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None; // typed text, not an address
    }
    let url = if v.contains("://") {
        v
    } else {
        format!("https://{v}")
    };
    (url.starts_with("https://") || url.starts_with("http://")).then_some(url)
}

/// The program a top-level window belongs to, if it is one that may be moved.
fn app_of_window(root: HWND) -> Option<AppRef> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let got = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        got.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let exe = path.rsplit('\\').next()?.to_string();
        if exe.len() <= 4 || !exe.to_ascii_lowercase().ends_with(".exe") {
            return None;
        }
        let app = AppRef {
            name: exe[..exe.len() - 4].to_string(),
            exe,
            url: None,
        };
        app.is_sane().then_some(app)
    }
}

/// The program whose window is being grabbed at this point, if it may be moved: the
/// title bar of any program, or for a browser the strip along the top that holds the
/// tabs and the empty space beside them. Only the press is looked at here; nothing
/// happens unless the drag then reaches an edge.
fn window_app_at(x: i32, y: i32) -> Option<(isize, AppRef)> {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return None;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.0.is_null() {
            return None;
        }
        let app = app_of_window(root)?;
        let mut grabbed = false;
        if app.is_browser() {
            let mut r = RECT::default();
            if GetWindowRect(root, &mut r).is_ok() {
                let strip = 48 * GetDpiForWindow(root) as i32 / 96;
                grabbed = y >= r.top && y - r.top <= strip;
            }
        }
        if !grabbed {
            let mut hit: usize = 0;
            let lparam =
                LPARAM((((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as i32 as isize);
            let ok = SendMessageTimeoutW(
                root,
                WM_NCHITTEST,
                WPARAM(0),
                lparam,
                SMTO_ABORTIFHUNG,
                100,
                Some(&mut hit),
            );
            grabbed = ok.0 != 0 && hit == 2; // HTCAPTION
        }
        grabbed.then_some((root.0 as isize, app))
    }
}

/// Left button went down: if it is on a file, find out which files; if it is on a
/// program's title bar, which program (both off the hook thread).
fn note_left_down(x: i32, y: i32) {
    let seq = DRAG_GEN.fetch_add(1, Relaxed) + 1;
    APP_STATE.store(0, Relaxed);
    if EDGES.load(Relaxed) == 0 {
        return;
    }
    if let Some(tx) = DRAG_TX.get() {
        if DRAG_FILES.load(Relaxed) {
            let _ = tx.send(DragReq::Sel { seq, x, y });
        }
        if MOVE_APPS.load(Relaxed) {
            let _ = tx.send(DragReq::App { seq, x, y });
        }
    }
}

/// The local drag has moved to the other computer: end it here without dropping anything.
fn cancel_local_drag() {
    let _ = std::thread::Builder::new()
        .name("tandem-dragcancel".into())
        .spawn(|| {
            send(&[
                key_input(VK_ESCAPE.0, 0, false, true),
                key_input(VK_ESCAPE.0, 0, false, false),
            ]);
            std::thread::sleep(std::time::Duration::from_millis(40));
            send(&[mouse_input(0, 0, 0, MOUSEEVENTF_LEFTUP)]);
        });
}

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;

const HOTKEY_VK: u32 = 0x91; // Scroll Lock: jump to the other computer / emergency exit

// Mouse motion is batched: a 1000 Hz mouse would otherwise put 1000 small packets a
// second on the Wi-Fi, which is what makes the far cursor stutter. Movement is sent
// at most every MOVE_EVERY_US, always in order with clicks and keys.
static GAME_MODE: AtomicBool = AtomicBool::new(true);
/// Set by the watcher thread while a full-screen app is in front (and game mode is on).
static GAME_ACTIVE: AtomicBool = AtomicBool::new(false);
static SWITCH_DELAY_MS: AtomicU32 = AtomicU32::new(0);
/// When the cursor first touched the edge in the current push (µs, 0 = not pushing).
static EDGE_SINCE_US: AtomicU64 = AtomicU64::new(0);

/// Window class of the foreground window ("CabinetWClass" is File Explorer).
pub fn foreground_class() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// True when the foreground window covers a whole monitor and has no title bar: games,
/// F11 browsers, full-screen video, presentations. A maximised normal window keeps its
/// title bar and the taskbar, so it never counts. (`SHQueryUserNotificationState`'s
/// "busy" answer is deliberately not used: it reports far too many false positives.)
pub fn fullscreen_foreground() -> bool {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        let mut name = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut name);
        let class = String::from_utf16_lossy(&name[..n.max(0) as usize]);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == WS_CAPTION.0 {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        r.left <= mi.rcMonitor.left
            && r.top <= mi.rcMonitor.top
            && r.right >= mi.rcMonitor.right
            && r.bottom >= mi.rcMonitor.bottom
    }
}

/// Polls whether a full-screen app is in front (game mode).
fn game_watcher_thread() {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let on = GAME_MODE.load(Relaxed) && fullscreen_foreground();
        GAME_ACTIVE.store(on, Relaxed);
    }
}

static PEND_DX: AtomicI32 = AtomicI32::new(0);
static PEND_DY: AtomicI32 = AtomicI32::new(0);
static LAST_FLUSH_US: AtomicU64 = AtomicU64::new(0);
const MOVE_EVERY_US: u64 = 4000;

fn now_us() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

fn flush_move() {
    let dx = PEND_DX.swap(0, Relaxed);
    let dy = PEND_DY.swap(0, Relaxed);
    if dx != 0 || dy != 0 {
        emit(CaptureEvent::Input(InputEvent::MouseMove { dx, dy }));
    }
    LAST_FLUSH_US.store(now_us(), Relaxed);
}

fn reset_move() {
    PEND_DX.store(0, Relaxed);
    PEND_DY.store(0, Relaxed);
}

/// Sends the tail of a movement when the mouse stops between two batches.
fn flusher_thread() {
    loop {
        if !CAPTURING.load(Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(40));
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        let pending = PEND_DX.load(Relaxed) != 0 || PEND_DY.load(Relaxed) != 0;
        if pending && now_us().saturating_sub(LAST_FLUSH_US.load(Relaxed)) >= MOVE_EVERY_US {
            flush_move();
        }
    }
}

fn emit(ev: CaptureEvent) {
    if let Some(s) = SINK.get() {
        let _ = s.send(ev);
    }
}

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
        if msg == WM_LBUTTONDOWN {
            note_left_down(h.x, h.y);
            return false;
        }
        if msg == WM_LBUTTONUP {
            DRAG_GEN.fetch_add(1, Relaxed);
            emit(CaptureEvent::LocalLeftUp);
            return false;
        }
        let mask = EDGES.load(Relaxed);
        if mask == 0 || msg != WM_MOUSEMOVE {
            return false;
        }
        // Which of the active edges, if any, is the cursor pushing against? A corner
        // (both edges at once) wins over the two edges that make it up.
        let at_l = h.x <= s.x;
        let at_r = h.x >= s.x + s.width - 1;
        let at_t = h.y <= s.y;
        let at_b = h.y >= s.y + s.height - 1;
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
        // Dragging a window or selection along the edge must not switch computers.
        let (left, right) = unsafe {
            (
                GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0,
                GetAsyncKeyState(VK_RBUTTON.0 as i32) < 0,
            )
        };
        // Holding files (picked up in Explorer or on the desktop) is the one case where a
        // pressed button may still cross.
        let carrying = hit != 0
            && left
            && !right
            && DRAG_FILES.load(Relaxed)
            && (CARRYING.load(Relaxed)
                || (DRAG_READY_GEN.load(Relaxed) == DRAG_GEN.load(Relaxed)
                    && DRAG_PATHS
                        .try_lock()
                        .map(|p| !p.is_empty())
                        .unwrap_or(false)));
        // A program's window held by its title bar (or a browser tab) may be held at the
        // edge with the button down too, to offer it to the other computer.
        let files_carry = carrying;
        let app_state = if hit != 0
            && left
            && !right
            && MOVE_APPS.load(Relaxed)
            && APP_GEN.load(Relaxed) == DRAG_GEN.load(Relaxed)
        {
            APP_STATE.load(Relaxed)
        } else {
            0
        };
        let app_wanted = !files_carry && (app_state == 1 || app_state == 2);
        let carrying = files_carry || app_wanted;
        let button_down = (left || right) && !carrying;
        if hit == 0 || button_down || GAME_ACTIVE.load(Relaxed) {
            AT_BIT.store(0, Relaxed);
            AT_EDGE.store(0, Relaxed);
            EDGE_SINCE_US.store(0, Relaxed);
            return false;
        }
        // Sliding from one edge to another (a corner) starts the push over.
        if AT_BIT.swap(hit, Relaxed) != hit {
            AT_EDGE.store(0, Relaxed);
            EDGE_SINCE_US.store(0, Relaxed);
        }
        let now = now_us().max(1);
        if EDGE_SINCE_US.load(Relaxed) == 0 {
            EDGE_SINCE_US.store(now, Relaxed);
        }
        if AT_EDGE.fetch_add(1, Relaxed) + 1 < PUSH_EVENTS {
            return false;
        }
        // Optional hold-to-cross delay, to stop accidental switches.
        let mut delay_us = SWITCH_DELAY_MS.load(Relaxed) as u64 * 1000;
        if app_wanted {
            delay_us = delay_us.max(APP_DWELL_US);
        }
        if now.saturating_sub(EDGE_SINCE_US.load(Relaxed)) < delay_us {
            return false;
        }
        // The browser's page is still being read: keep holding, the next move asks again.
        if app_wanted && app_state == 1 {
            return false;
        }
        AT_BIT.store(0, Relaxed);
        AT_EDGE.store(0, Relaxed);
        EDGE_SINCE_US.store(0, Relaxed);
        let fx = (h.x - s.x) as f32 / (s.width - 1).max(1) as f32;
        let fy = (h.y - s.y) as f32 / (s.height - 1).max(1) as f32;
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
        if app_wanted {
            // Not a crossing: the cursor stays here. Put the dragged window back (Esc ends
            // its move) and offer it to the computer on that side.
            let app = APP_DRAG.try_lock().ok().and_then(|mut a| a.take());
            APP_STATE.store(0, Relaxed);
            DRAG_GEN.fetch_add(1, Relaxed);
            if let Some(app) = app {
                emit(CaptureEvent::AppDragged { edge, app });
                cancel_local_drag();
            }
            return false;
        }
        reset_move();
        set_capturing(true);
        let (cx, cy) = s.center();
        unsafe {
            let _ = SetCursorPos(cx, cy);
        }
        if carrying {
            let paths = DRAG_PATHS
                .try_lock()
                .map(|mut p| std::mem::take(&mut *p))
                .unwrap_or_default();
            if !paths.is_empty() {
                DRAG_GEN.fetch_add(1, Relaxed);
                emit(CaptureEvent::DragCrossed { edge, frac, paths });
                cancel_local_drag();
                return true;
            }
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
            PEND_DX.fetch_add(dx, Relaxed);
            PEND_DY.fetch_add(dy, Relaxed);
            if now_us().saturating_sub(LAST_FLUSH_US.load(Relaxed)) >= MOVE_EVERY_US {
                flush_move();
            }
            None
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
        flush_move(); // keep clicks and wheel in order with the motion before them
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
            if set_capturing(false) {
                // Emergency exit: give the mouse back *here*, synchronously.
                let (cx, cy) = vscreen().center();
                unsafe {
                    let _ = SetCursorPos(cx, cy);
                }
                emit(CaptureEvent::Failsafe);
            } else if EDGES.load(Relaxed) != 0 {
                emit(CaptureEvent::HotkeyEngage);
            }
        }
        return true;
    }
    if !CAPTURING.load(Relaxed) {
        return false;
    }
    flush_move();
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

// ------------------------------------------------------------------ hidden cursor

/// While the other computer is being driven, the pointer on this one is parked in the
/// middle of the screen. A parked pointer looks like a stuck mouse, so it is hidden by
/// swapping every system cursor for a blank one, and put back when control returns.
/// The work happens on its own thread: the mouse hook must never wait.
static CURSOR_TX: OnceLock<std::sync::mpsc::Sender<bool>> = OnceLock::new();

/// Every standard cursor shape (OCR_NORMAL, IBEAM, WAIT, CROSS, UP, SIZENWSE, SIZENESW,
/// SIZEWE, SIZENS, SIZEALL, NO, HAND, APPSTARTING).
const CURSOR_IDS: [u32; 13] = [
    32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650,
];

/// Puts the user's own cursor scheme back. Safe to call any time; also called at startup
/// so a crash while the pointer was hidden can never leave it invisible.
pub fn restore_cursors() {
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETCURSORS,
            0,
            None,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
}

fn hide_cursors() {
    unsafe {
        let hinst = GetModuleHandleW(None).ok().map(|h| HINSTANCE(h.0));
        // A 32x32 monochrome cursor: AND mask all ones, XOR mask all zeros = transparent.
        let and_plane = [0xFFu8; 32 * 32 / 8];
        let xor_plane = [0u8; 32 * 32 / 8];
        for id in CURSOR_IDS {
            // SetSystemCursor takes ownership of the handle, so each shape needs its own.
            if let Ok(c) = CreateCursor(
                hinst,
                0,
                0,
                32,
                32,
                and_plane.as_ptr() as *const _,
                xor_plane.as_ptr() as *const _,
            ) {
                let _ = SetSystemCursor(c, SYSTEM_CURSOR_ID(id));
            }
        }
    }
}

fn cursor_thread(rx: std::sync::mpsc::Receiver<bool>) {
    restore_cursors();
    let mut hidden = false;
    while let Ok(mut want) = rx.recv() {
        // Only the latest wish matters.
        while let Ok(w) = rx.try_recv() {
            want = w;
        }
        if want && !hidden {
            hide_cursors();
            hidden = true;
        } else if !want && hidden {
            restore_cursors();
            hidden = false;
        }
    }
    if hidden {
        restore_cursors();
    }
}

// ------------------------------------------------------------------ protected screens

/// True while Windows shows a screen no ordinary program may type into: the UAC admin
/// prompt, the lock screen, Ctrl+Alt+Del. The input desktop can't even be opened then.
fn input_desktop_blocked() -> bool {
    unsafe {
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) {
            Ok(h) => {
                let _ = CloseDesktop(h);
                false
            }
            Err(_) => true,
        }
    }
}

fn desktop_watcher_thread() {
    let mut blocked = false;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let now = input_desktop_blocked();
        if now && !blocked {
            // Anyone who was driving gives the keyboard and mouse back.
            if CAPTURING.load(Relaxed) {
                // Hooks don't see the protected screen, so stop swallowing here too.
                set_capturing(false);
            }
            emit(CaptureEvent::InputBlocked);
        }
        blocked = now;
    }
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
        // 1 ms timers: without this Windows sleeps in 15.6 ms steps and every
        // timed wait in the app (batching, polling) turns choppy.
        unsafe {
            timeBeginPeriod(1);
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

    fn set_active_edges(&self, edges: &[Edge]) {
        EDGES.store(edge_mask(edges), Relaxed);
        AT_BIT.store(0, Relaxed);
        AT_EDGE.store(0, Relaxed);
    }

    fn begin_capture(&self) {
        reset_move();
        let (cx, cy) = vscreen().center();
        unsafe {
            let _ = SetCursorPos(cx, cy);
        }
        AT_EDGE.store(0, Relaxed);
        set_capturing(true);
    }

    fn end_capture(&self, x: i32, y: i32) {
        set_capturing(false);
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
                // Round up so the 0..65535 grid never lands one pixel short: with
                // flooring, 1-pixel moves are swallowed and the cursor feels sticky.
                let w = s.width.max(1) as i64;
                let h = s.height.max(1) as i64;
                let ax = (((nx - s.x) as i64 * 65536 + w - 1) / w).min(65535) as i32;
                let ay = (((ny - s.y) as i64 * 65536 + h - 1) / h).min(65535) as i32;
                send(&[mouse_input(
                    ax,
                    ay,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )]);
                // Windows' own conversion differs slightly between versions and DPI
                // setups; if it still missed the pixel, correct it exactly.
                let mut q = POINT::default();
                unsafe {
                    if GetCursorPos(&mut q).is_ok() && (q.x != nx || q.y != ny) {
                        let _ = SetCursorPos(nx, ny);
                    }
                }
            }
            InputEvent::Button { button, down } => {
                if *button == Button::Left {
                    // The other computer's mouse is the one dragging here: our own hook
                    // never sees injected clicks, so look at what was picked up now.
                    if *down {
                        let mut p = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut p);
                        }
                        note_left_down(p.x, p.y);
                    } else {
                        DRAG_GEN.fetch_add(1, Relaxed);
                    }
                }
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
                ClipContent::Image(png) => {
                    // The classic bitmap first: Paint, Word, Discord and most apps
                    // paste from it, and Windows derives the other bitmap formats.
                    if let Some(dib) = crate::image::png_to_dib(png) {
                        put(CF_DIB, &dib);
                    }
                    // And the PNG, for apps that read it (keeps transparency).
                    let fmt = RegisterClipboardFormatW(w!("PNG"));
                    if fmt != 0 {
                        put(fmt, png);
                    }
                }
            }
            let _ = CloseClipboard();
        }
    }

    fn set_game_mode(&self, on: bool) {
        GAME_MODE.store(on, Relaxed);
        if !on {
            GAME_ACTIVE.store(false, Relaxed);
        }
    }

    fn set_switch_delay(&self, ms: u32) {
        SWITCH_DELAY_MS.store(ms, Relaxed);
    }

    fn set_drag_files(&self, on: bool) {
        DRAG_FILES.store(on, Relaxed);
        if on {
            ensure_drag_thread();
            if let Some(tx) = DRAG_TX.get() {
                let _ = tx.send(DragReq::Warm);
            }
        } else if let Some(tx) = DRAG_TX.get() {
            let _ = tx.send(DragReq::Stop);
        }
    }

    fn set_move_apps(&self, on: bool) {
        MOVE_APPS.store(on, Relaxed);
        APP_STATE.store(0, Relaxed);
        if on {
            ensure_drag_thread();
            // Starts the helper now, so the first browser page is read quickly.
            if let Some(tx) = DRAG_TX.get() {
                let _ = tx.send(DragReq::Warm);
            }
        }
    }

    fn confirm_move_app(&self, to: &str, app: &AppRef) -> bool {
        let text = match &app.url {
            Some(u) => {
                let shown: String = u.chars().take(160).collect();
                format!("Open this page on {to}?\n\n{shown}")
            }
            None if app.may_close() => format!(
                "Move {} to {to}?\n\n{} will open on {to} and close on this computer.",
                app.name, app.name
            ),
            None => format!(
                "Open {} on {to}?\n\nIt stays open on this computer too.",
                app.name
            ),
        };
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(text),
                &HSTRING::from("CubeCursor"),
                MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
            ) == IDYES
        }
    }

    fn launch_app(&self, app: &AppRef) -> Result<(), String> {
        if !app.is_sane() {
            return Err("That app can't be moved".into());
        }
        if let Some(url) = &app.url {
            return shell_open(url).map_err(|_| "Couldn't open the page on that computer".into());
        }
        match find_program(app) {
            Some(l) => shell_open_with(&l.file, l.params.as_deref())
                .map_err(|_| format!("{} wouldn't start on that computer", app.name)),
            None => Err(format!("{} isn't installed on that computer", app.name)),
        }
    }

    fn close_app(&self, app: &AppRef) {
        use std::os::windows::process::CommandExt;
        if !app.is_sane() || !app.may_close() {
            return;
        }
        // Chat and music programs hide in the tray when their window is closed, so the
        // whole program is ended.
        let _ = std::process::Command::new("taskkill")
            .args(["/IM", &app.exe, "/F"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output();
    }

    fn take_drag_files(&self) -> Option<Vec<PathBuf>> {
        if !DRAG_FILES.load(Relaxed) || DRAG_READY_GEN.load(Relaxed) != DRAG_GEN.load(Relaxed) {
            return None;
        }
        let mut p = DRAG_PATHS.lock().ok()?;
        let v = std::mem::take(&mut *p);
        (!v.is_empty()).then_some(v)
    }

    fn cancel_drag(&self) {
        cancel_local_drag();
    }

    fn set_carrying(&self, on: bool) {
        CARRYING.store(on, Relaxed);
    }

    fn drop_dir_at(&self, x: i32, y: i32) -> Option<PathBuf> {
        if !DRAG_FILES.load(Relaxed) {
            return None;
        }
        let kind = shell_kind_at(x, y, false)?;
        let (rtx, rrx) = std::sync::mpsc::channel();
        DRAG_TX
            .get()?
            .send(DragReq::Dir { kind, reply: rtx })
            .ok()?;
        let reply = rrx
            .recv_timeout(std::time::Duration::from_millis(2000))
            .ok()??;
        let dir = PathBuf::from(reply.strip_prefix("OK|")?);
        dir.is_dir().then_some(dir)
    }

    fn set_capture_sink(&self, tx: UnboundedSender<CaptureEvent>) {
        if SINK.set(tx).is_ok() {
            let _ = std::thread::Builder::new()
                .name("tandem-game".into())
                .spawn(game_watcher_thread);
            if let Err(e) = std::thread::Builder::new()
                .name("tandem-input".into())
                .spawn(hook_thread)
            {
                log::error!("cannot start input thread: {e}");
            }
            let _ = std::thread::Builder::new()
                .name("tandem-flush".into())
                .spawn(flusher_thread);
            let (tx, rx) = std::sync::mpsc::channel();
            if CURSOR_TX.set(tx).is_ok() {
                let _ = std::thread::Builder::new()
                    .name("tandem-cursor".into())
                    .spawn(move || cursor_thread(rx));
            }
            let _ = std::thread::Builder::new()
                .name("tandem-desktop".into())
                .spawn(desktop_watcher_thread);
        }
    }
}

/// Runs on the clipboard worker thread (never on the engine), so it can afford to wait
/// for an app that is still holding the clipboard open — Explorer does for a moment
/// after a copy.
unsafe fn open_clipboard() -> bool {
    for _ in 0..30 {
        if OpenClipboard(None).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
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
    // Screenshots and "copy image": apps that offer the compressed "PNG" format.
    let png_fmt = RegisterClipboardFormatW(w!("PNG"));
    if png_fmt != 0 && IsClipboardFormatAvailable(png_fmt).is_ok() {
        let h = GetClipboardData(png_fmt).ok()?;
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, GlobalSize(g)).to_vec();
        let _ = GlobalUnlock(g);
        return Some(ClipContent::Image(bytes));
    }
    // Everything else that is a picture (Paint, Snipping Tool, "copy image" in many
    // apps) is a device-independent bitmap.
    for fmt in [CF_DIBV5, CF_DIB] {
        if IsClipboardFormatAvailable(fmt).is_err() {
            continue;
        }
        let Ok(h) = GetClipboardData(fmt) else {
            continue;
        };
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            continue;
        }
        let dib = std::slice::from_raw_parts(p, GlobalSize(g)).to_vec();
        let _ = GlobalUnlock(g);
        if let Some(png) = crate::image::dib_to_png(&dib) {
            return Some(ClipContent::Image(png));
        }
    }
    None
}
