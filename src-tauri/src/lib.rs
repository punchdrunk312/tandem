//! Tandem desktop shell: a tray app around `tandem-core`.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tandem_core::node::*;
use tandem_core::proto::{Edge, DEFAULT_TCP_PORT, DISCOVERY_PORT};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, Theme, WindowEvent};
use tauri::{WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

mod elevate;
mod ghost;
mod island;
mod license_client;
mod reports;
mod shellmenu;
#[cfg(windows)]
mod sound;
use island::{IslandRect, PollGate};

struct Shared {
    gate: Arc<PollGate>,
    ui: std::sync::Mutex<UiOptions>,
    ui_path: PathBuf,
    /// Paths from the Explorer "Send with Tandem" entry, collected for a moment so a
    /// multi-selection goes out as one transfer.
    send_queue: std::sync::Mutex<Vec<PathBuf>>,
    send_armed: std::sync::atomic::AtomicBool,
}

/// Top-bar preferences (kept in `ui.json`, separate from the pairing data).
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct UiOptions {
    /// Show the top bar at all.
    #[serde(default = "yes")]
    enabled: bool,
    /// The bar sleeps while a full-screen app (game, F11 video) is in front.
    #[serde(default = "yes")]
    hide_fullscreen: bool,
    /// Follow the cursor to whichever display it is on.
    #[serde(default)]
    monitor_cursor: bool,
    /// Send error reports automatically: None until the person has been asked.
    #[serde(default)]
    error_reports: Option<bool>,
}

fn yes() -> bool {
    true
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            hide_fullscreen: true,
            monitor_cursor: false,
            error_reports: None,
        }
    }
}

struct AppState {
    node: Node,
    download_dir: PathBuf,
}

// ------------------------------------------------------------------ storage

/// Device list on disk; the 32-byte link keys go to the Windows Credential
/// Manager (and stay in the JSON file on other OSes, for development only).
struct AppStore {
    path: PathBuf,
}

#[cfg(windows)]
mod secret {
    use keyring::Entry;
    fn entry(id: &str) -> Option<Entry> {
        Entry::new("Tandem", &format!("link-{id}")).ok()
    }
    pub fn set(id: &str, v: &str) {
        if let Some(e) = entry(id) {
            let _ = e.set_password(v);
        }
    }
    pub fn get(id: &str) -> Option<String> {
        entry(id)?.get_password().ok()
    }
    pub fn delete(id: &str) {
        if let Some(e) = entry(id) {
            let _ = e.delete_credential();
        }
    }
}

impl Store for AppStore {
    fn load(&self) -> Option<Persisted> {
        #[allow(unused_mut)]
        let mut p: Persisted = serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()?;
        #[cfg(windows)]
        p.peers.retain_mut(|peer| match secret::get(&peer.id) {
            Some(k) => {
                peer.link_key = k;
                true
            }
            None => false, // key lost (e.g. different Windows user): must pair again
        });
        Some(p)
    }

    fn save(&self, p: &Persisted) {
        #[allow(unused_mut)]
        let mut q = p.clone();
        #[cfg(windows)]
        {
            // Forget keys of devices that were removed since the last save.
            if let Some(old) = std::fs::read(&self.path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Persisted>(&b).ok())
            {
                for o in &old.peers {
                    if !q.peers.iter().any(|n| n.id == o.id) {
                        secret::delete(&o.id);
                    }
                }
            }
            for peer in &mut q.peers {
                secret::set(&peer.id, &peer.link_key);
                peer.link_key.clear();
            }
        }
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec_pretty(&q) {
            let _ = std::fs::write(&self.path, b);
        }
    }
}

// ------------------------------------------------------------------ commands

type R = Result<(), String>;

#[tauri::command]
fn snapshot(st: State<'_, AppState>) -> Snapshot {
    st.node.snapshot()
}

fn license_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_config_dir().map(|p| p.join("license.json")).map_err(|e| e.to_string())
}

#[tauri::command]
fn license_status(app: AppHandle) -> Result<license_client::LicenseView, String> {
    Ok(license_client::status(&license_path(&app)?))
}

#[tauri::command(async)]
fn license_activate(app: AppHandle, st: State<'_, AppState>, key: String, takeover: Option<bool>) -> Result<license_client::LicenseView, String> {
    license_client::activate(&license_path(&app)?, &st.node, key, takeover.unwrap_or(false))
}

#[tauri::command(async)]
fn license_trial(app: AppHandle, st: State<'_, AppState>) -> Result<license_client::LicenseView, String> {
    license_client::start_trial(&license_path(&app)?, &st.node)
}

#[tauri::command(async)]
fn license_release(app: AppHandle, st: State<'_, AppState>) -> Result<license_client::LicenseView, String> {
    license_client::release(&license_path(&app)?, &st.node)
}

#[tauri::command]
fn connect(st: State<'_, AppState>, addr: String, port: Option<u16>, code: String) -> R {
    let ip: IpAddr = addr
        .trim()
        .parse()
        .map_err(|_| "That is not a valid IP address".to_string())?;
    st.node.connect(ip, port.unwrap_or(DEFAULT_TCP_PORT), code);
    Ok(())
}

#[tauri::command]
fn connect_known(st: State<'_, AppState>, id: String) {
    st.node.connect_known(id);
}

/// Disconnects one computer, or all of them when `peer_id` is not given.
#[tauri::command]
fn disconnect(st: State<'_, AppState>, peer_id: Option<String>) {
    match peer_id {
        Some(id) => st.node.disconnect_peer(id),
        None => st.node.disconnect(),
    }
}

#[tauri::command]
fn set_side(st: State<'_, AppState>, peer_id: String, side: String) -> R {
    let side = Edge::parse(&side).ok_or("bad side")?;
    st.node.set_side(peer_id, side);
    Ok(())
}

#[tauri::command]
fn forget(st: State<'_, AppState>, id: String) {
    st.node.forget(id);
}

/// Sends to `peer_id`; without it: to the computer being controlled, or to all of them.
fn send_to(node: &Node, paths: Vec<PathBuf>, peer_id: Option<String>) {
    match peer_id {
        Some(id) => node.send_files_to(paths, id),
        None => node.send_files(paths),
    }
}

#[tauri::command]
fn send_files(st: State<'_, AppState>, paths: Vec<String>, peer_id: Option<String>) {
    send_to(
        &st.node,
        paths.into_iter().map(PathBuf::from).collect(),
        peer_id,
    );
}

#[tauri::command]
async fn pick_and_send(app: AppHandle, peer_id: Option<String>) -> R {
    let a2 = app.clone();
    let picked =
        tauri::async_runtime::spawn_blocking(move || a2.dialog().file().blocking_pick_files())
            .await
            .map_err(|e| e.to_string())?;
    if let Some(files) = picked {
        let paths: Vec<PathBuf> = files
            .into_iter()
            .filter_map(|f| f.into_path().ok())
            .collect();
        if !paths.is_empty() {
            send_to(&app.state::<AppState>().node, paths, peer_id);
        }
    }
    Ok(())
}

#[tauri::command]
fn set_clipboard_sync(st: State<'_, AppState>, on: bool) {
    st.node.set_clipboard_sync(on);
}

#[tauri::command]
fn ghost_show(app: AppHandle, on: bool) {
    ghost::set(&app, on);
}

#[tauri::command]
fn set_drag_files(st: State<'_, AppState>, on: bool) {
    st.node.set_drag_files(on);
}

#[tauri::command]
fn set_move_apps(st: State<'_, AppState>, on: bool) {
    st.node.set_move_apps(on);
}

#[tauri::command]
fn set_game_mode(st: State<'_, AppState>, on: bool) {
    st.node.set_game_mode(on);
}

#[tauri::command]
fn set_switch_delay(st: State<'_, AppState>, ms: u32) {
    st.node.set_switch_delay(ms);
}

#[tauri::command]
fn ui_get(sh: State<'_, Shared>) -> UiOptions {
    sh.ui.lock().unwrap().clone()
}

#[tauri::command]
fn ui_set(app: AppHandle, sh: State<'_, Shared>, opts: UiOptions) {
    reports::set_enabled(opts.error_reports == Some(true));
    apply_ui(&app, &sh.gate, &opts);
    if let Ok(b) = serde_json::to_vec_pretty(&opts) {
        let _ = std::fs::write(&sh.ui_path, b);
    }
    *sh.ui.lock().unwrap() = opts;
}

fn apply_ui(app: &AppHandle, gate: &PollGate, o: &UiOptions) {
    use std::sync::atomic::Ordering::Relaxed;
    gate.hide_fullscreen.store(o.hide_fullscreen, Relaxed);
    gate.monitor_cursor.store(o.monitor_cursor, Relaxed);
    if let Some(w) = island::window(app) {
        if o.enabled {
            island::apply_geometry(app, gate.pref(), gate.collapsed.load(Relaxed));
            island::make_non_activating(&w);
            let _ = w.show();
            gate.set_active(true);
        } else {
            gate.set_active(false);
            let _ = w.hide();
        }
    }
}

#[tauri::command]
fn set_name(st: State<'_, AppState>, name: String) {
    st.node.set_name(name);
}

#[tauri::command]
fn new_code(st: State<'_, AppState>) {
    st.node.new_pairing_code();
}

/// Jumps to one computer, or to the one used last when `peer_id` is not given.
#[tauri::command]
fn engage(st: State<'_, AppState>, peer_id: Option<String>) {
    match peer_id {
        Some(id) => st.node.engage_peer(id),
        None => st.node.engage(),
    }
}

/// Open the folder where received files go.
#[tauri::command]
fn open_downloads(st: State<'_, AppState>) {
    let _ = std::fs::create_dir_all(&st.download_dir);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer")
        .arg(&st.download_dir)
        .spawn();
}

#[tauri::command]
fn autostart_get(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn autostart_set(app: AppHandle, on: bool) -> R {
    let al = app.autolaunch();
    (if on { al.enable() } else { al.disable() }).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ dropped files

/// Files dropped on a window arrive as bytes (HTML5 drop). They are copied into a
/// staging folder and sent from there.
fn stage_root() -> PathBuf {
    std::env::temp_dir().join("Tandem").join("out")
}

fn unhex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// A relative path made only of plain names: no `..`, drive letters or backslashes.
fn safe_rel(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
        {
            return None;
        }
        out.push(part);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn safe_id(id: &str) -> Option<PathBuf> {
    (!id.is_empty() && id.len() < 40 && id.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| stage_root().join(id))
}

#[tauri::command]
fn stage_begin() -> Result<String, String> {
    let id = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    std::fs::create_dir_all(stage_root().join(&id)).map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
fn stage_chunk(request: tauri::ipc::Request<'_>) -> R {
    use std::io::{Seek, SeekFrom, Write};
    let h = request.headers();
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let dir = get("x-session")
        .and_then(|s| safe_id(&s))
        .ok_or("bad session")?;
    let rel = get("x-path")
        .and_then(|p| unhex(&p))
        .and_then(|p| safe_rel(&p))
        .ok_or("bad path")?;
    let off: u64 = get("x-offset")
        .and_then(|o| o.parse().ok())
        .ok_or("bad offset")?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected raw bytes".into());
    };
    let path = dir.join(rel);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    f.write_all(bytes).map_err(|e| e.to_string())
}

#[tauri::command]
fn stage_finish(
    st: State<'_, AppState>,
    id: String,
    names: Vec<String>,
    peer_id: Option<String>,
) -> R {
    let dir = safe_id(&id).ok_or("bad session")?;
    let paths: Vec<PathBuf> = names
        .iter()
        .filter_map(|n| safe_rel(n))
        .map(|n| dir.join(n))
        .filter(|p| p.exists())
        .collect();
    if paths.is_empty() {
        return Err("nothing to send".into());
    }
    send_to(&st.node, paths, peer_id);
    Ok(())
}

/// Sounds the island asked us to play, as 16-bit mono 44.1 kHz samples.
static SOUNDS: std::sync::Mutex<Vec<(String, Vec<i16>)>> = std::sync::Mutex::new(Vec::new());

/// The island hands over one rendered sound (raw little-endian i16 body, name in a header).
#[tauri::command]
fn sound_register(request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected raw bytes".into());
    };
    let name = request
        .headers()
        .get("x-sound")
        .and_then(|v| v.to_str().ok())
        .ok_or("missing sound name")?
        .to_string();
    let pcm: Vec<i16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();
    let mut all = SOUNDS.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(n, _)| *n != name);
    all.push((name, pcm));
    Ok(())
}

/// Plays a registered sound through Windows (a new sound replaces the one still playing).
#[tauri::command]
fn sound_play(name: String, volume: f32) {
    #[cfg(windows)]
    {
        let pcm = {
            let all = SOUNDS.lock().unwrap_or_else(|e| e.into_inner());
            match all.iter().find(|(n, _)| *n == name) {
                Some((_, p)) => p.clone(),
                None => return,
            }
        };
        sound::play(&pcm, volume.clamp(0.0, 1.0));
    }
    #[cfg(not(windows))]
    let _ = (name, volume);
}

/// Debug trail for the island's page (JS errors, launch) — `tandem.log` in the config folder.
#[tauri::command]
fn log_js(app: AppHandle, msg: String) {
    log_line(&app, &format!("js: {msg}"));
}

fn log_line(app: &AppHandle, msg: &str) {
    use std::io::Write;
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("tandem.log");
    // Keep it small: start over past 256 KB.
    if std::fs::metadata(&path)
        .map(|m| m.len() > 262_144)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

/// Sends the engine's own notes (clipboard, app moves…) to the same tandem.log, so a
/// problem on someone's PC can be read afterwards.
struct FileLog(std::path::PathBuf);
impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info && m.target().starts_with("tandem")
    }
    fn log(&self, r: &log::Record) {
        use std::io::Write;
        if !self.enabled(r.metadata()) {
            return;
        }
        reports::note(r.level(), &r.args().to_string());
        if std::fs::metadata(&self.0)
            .map(|m| m.len() > 262_144)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&self.0);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
        {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "[{secs}] {} {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

fn start_file_log(app: &AppHandle) {
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let logger: &'static FileLog = Box::leak(Box::new(FileLog(dir.join("tandem.log"))));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

// ------------------------------------------------------------------ island

#[tauri::command]
fn set_collapsed(app: AppHandle, sh: State<'_, Shared>, collapsed: bool) {
    sh.gate
        .collapsed
        .store(collapsed, std::sync::atomic::Ordering::Relaxed);
    island::apply_geometry(&app, sh.gate.pref(), collapsed);
    sh.gate.forget_ignore_state();
    if let Some(w) = island::window(&app) {
        log_line(
            &app,
            &format!(
                "set_collapsed({collapsed}) -> size={:?} pos={:?}",
                w.outer_size().ok(),
                w.outer_position().ok()
            ),
        );
    }
}

#[tauri::command]
fn set_island_rect(sh: State<'_, Shared>, x: f64, y: f64, width: f64, height: f64) {
    sh.gate.set_rect(IslandRect {
        x,
        y,
        w: width,
        h: height,
    });
}

/// The page closed the bar: a full-screen app gets its screen back at once.
#[tauri::command]
fn end_reveal(sh: State<'_, Shared>) {
    island::end_reveal(&sh.gate);
}

#[derive(serde::Serialize)]
struct ElevState {
    /// This copy of Tandem is running as administrator.
    elevated: bool,
    /// Tandem starts as administrator when Windows starts.
    autostart: bool,
}

#[derive(serde::Serialize)]
struct PlatformInfo {
    os: &'static str,
    perms_ok: bool,
}

/// Which system this is, and (on a Mac) whether its privacy permissions are given.
#[tauri::command]
fn platform_info() -> PlatformInfo {
    #[cfg(target_os = "macos")]
    {
        PlatformInfo {
            os: "macos",
            perms_ok: tandem_core::mac::permissions_granted(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        PlatformInfo {
            os: std::env::consts::OS,
            perms_ok: true,
        }
    }
}

/// Opens the page of System Settings where CubeCursor is allowed in (Mac only).
#[tauri::command]
fn open_mac_privacy(which: String) {
    #[cfg(target_os = "macos")]
    {
        tandem_core::mac::request_permissions();
        let pane = if which == "input" {
            "Privacy_ListenEvent"
        } else {
            "Privacy_Accessibility"
        };
        let _ = std::process::Command::new("open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{pane}"
            ))
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = which;
}

#[tauri::command]
fn elevation_state() -> ElevState {
    ElevState {
        elevated: elevate::is_elevated(),
        autostart: elevate::autostart_is_on(),
    }
}

/// Asks Windows once, then starts an elevated copy and quits this one.
#[tauri::command]
async fn restart_elevated(app: AppHandle) -> R {
    if elevate::is_elevated() {
        return Ok(());
    }
    // `Start-Process -Verb RunAs` blocks until the user answers the UAC question.
    tauri::async_runtime::spawn_blocking(elevate::spawn_elevated_copy)
        .await
        .map_err(|e| e.to_string())??;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn elevated_autostart_set(app: AppHandle, on: bool) -> R {
    elevate::autostart_set(on)?;
    if on {
        // Two copies starting at logon would fight over the single-instance lock.
        let _ = app.autolaunch().disable();
    }
    Ok(())
}

#[tauri::command]
fn shell_menu_get() -> bool {
    shellmenu::is_on()
}

#[tauri::command]
fn shell_menu_set(on: bool) -> R {
    shellmenu::set(on)
}

#[tauri::command]
fn reposition(app: AppHandle, sh: State<'_, Shared>) {
    let c = sh.gate.collapsed.load(std::sync::atomic::Ordering::Relaxed);
    island::apply_geometry(&app, sh.gate.pref(), c);
    sh.gate.forget_ignore_state();
}

#[tauri::command]
fn open_main_window(app: AppHandle) {
    show_main(&app);
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.state::<AppState>().node.disconnect();
    app.exit(0);
}

// ------------------------------------------------------------------ shell

/// A hotkey / tray entry for the bar. `what` is "bar" (control bar, Ctrl+Alt+Z) or
/// "pill" (transfer bar, Ctrl+Alt+U). If the bar is switched off in the settings there is
/// nothing to show: the control bar's key opens the settings window instead.
fn bar_hotkey(app: &AppHandle, what: &'static str) {
    let Some(sh) = app.try_state::<Shared>() else {
        return;
    };
    let enabled = sh.ui.lock().unwrap().enabled;
    if enabled {
        island::hotkey(app, &sh.gate, what);
    } else if what == "bar" {
        show_main(app);
    }
}

/// Paths after each `--send` on a command line (Explorer's "Send with Tandem").
fn send_args<I: IntoIterator<Item = String>>(args: I) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--send" {
            if let Some(p) = it.next() {
                out.push(PathBuf::from(p));
            }
        }
    }
    out
}

/// Collects `--send` paths for a moment (Explorer starts one process per selected item),
/// then sends them together. `patience` is how long to wait for a connection first —
/// Tandem may have just been started by the right-click itself.
fn queue_sends(app: &AppHandle, paths: Vec<PathBuf>, patience: Duration) {
    use std::sync::atomic::Ordering::AcqRel;
    if paths.is_empty() {
        return;
    }
    let Some(sh) = app.try_state::<Shared>() else {
        return;
    };
    sh.send_queue.lock().unwrap().extend(paths);
    if sh.send_armed.swap(true, AcqRel) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let connected = |app: &AppHandle| {
            app.try_state::<AppState>()
                .map(|st| st.node.snapshot().paired.iter().any(|p| p.connected))
                .unwrap_or(false)
        };
        let t0 = std::time::Instant::now();
        while !connected(&app) && t0.elapsed() < patience {
            std::thread::sleep(Duration::from_millis(250));
        }
        let Some(sh) = app.try_state::<Shared>() else {
            return;
        };
        let paths = std::mem::take(&mut *sh.send_queue.lock().unwrap());
        sh.send_armed
            .store(false, std::sync::atomic::Ordering::Release);
        if paths.is_empty() {
            return;
        }
        if connected(&app) {
            app.state::<AppState>().node.send_files(paths);
        } else {
            let _ = app.emit_to(
                island::WINDOW_LABEL,
                "note",
                "Not connected — open CubeCursor on both computers first.",
            );
            island::hotkey(&app, &sh.gate, "bar");
        }
    });
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Keep in sync with `additionalBrowserArgs` in tauri.conf.json.
#[cfg(windows)]
const WEBVIEW_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// The pairing / settings window: created hidden, hides instead of closing.
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let b = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("settings.html".into()))
        .title("CubeCursor")
        .theme(Some(Theme::Dark))
        .inner_size(900.0, 660.0)
        .min_inner_size(640.0, 520.0)
        .center()
        .resizable(true)
        .visible(false)
        .disable_drag_drop_handler();
    // Every window of the app shares one WebView2 environment, so it must get the same
    // browser arguments as the island (see tauri.conf.json). The autoplay flag lets the
    // island play its sounds without the user having clicked inside it first.
    #[cfg(windows)]
    let b = b.additional_browser_args(WEBVIEW_ARGS);
    b.build()?;
    Ok(())
}

pub fn run() {
    // Started by "Restart as administrator": let the old copy quit first.
    elevate::wait_for_parent();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Explorer's "Send with Tandem" lands here; starting Tandem a second time
            // by hand brings the control bar up.
            let sends = send_args(args);
            if sends.is_empty() {
                bar_hotkey(app, "bar");
            } else {
                queue_sends(app, sends, Duration::ZERO);
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    match shortcut.key {
                        Code::KeyZ => bar_hotkey(app, "bar"),
                        Code::KeyU => bar_hotkey(app, "pill"),
                        _ => {}
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .setup(|app| {
            start_file_log(app.handle());
            let cfg_dir = app.path().app_config_dir()?;
            let download_dir = app
                .path()
                .download_dir()
                .unwrap_or_else(|_| cfg_dir.clone())
                .join("CubeCursor");
            let cfg = NodeConfig {
                tcp_port: DEFAULT_TCP_PORT,
                udp_port: DISCOVERY_PORT,
                temp_dir: std::env::temp_dir().join("Tandem"),
                download_dir: download_dir.clone(),
                discovery: true,
                default_name: std::env::var("COMPUTERNAME")
                    .unwrap_or_else(|_| "My computer".into()),
                // Licence checking stays off until the app can fetch and renew passes.
                license_key: None,
            };
            let node = tauri::async_runtime::block_on(Node::start(
                cfg,
                tandem_core::default_platform(),
                Arc::new(AppStore {
                    path: cfg_dir.join("state.json"),
                }),
            ))?;

            let gate = Arc::new(PollGate::new());

            // Push state to the UI, at most ~12 times a second.
            let mut rx = node.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut last_notice: Option<String> = None;
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    let s = rx.borrow_and_update().clone();
                    let _ = handle.emit("snapshot", &s);
                    // A new notice ("control came back because…") also shows on the bar.
                    if s.notice != last_notice {
                        if let Some(n) = &s.notice {
                            let _ = handle.emit_to(island::WINDOW_LABEL, "note", n);
                        }
                        last_notice = s.notice.clone();
                    }
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
            });
            license_client::start_renewal(cfg_dir.join("license.json"), node.clone());
            app.manage(AppState { node, download_dir });

            let _ = std::fs::remove_dir_all(stage_root());
            let ui_path = cfg_dir.join("ui.json");
            let ui: UiOptions = std::fs::read(&ui_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            reports::start(
                &cfg_dir,
                env!("CARGO_PKG_VERSION"),
                ui.error_reports == Some(true),
            );
            {
                // Which names to hide in reports: this computer's and the paired ones'.
                let h = app.handle().clone();
                let _ = std::thread::Builder::new()
                    .name("cubecursor-report-names".into())
                    .spawn(move || loop {
                        if let Some(st) = h.try_state::<AppState>() {
                            let s = st.node.snapshot();
                            let mut names = vec![s.device_name.clone()];
                            names.extend(s.paired.iter().map(|p| p.name.clone()));
                            names.extend(s.nearby.iter().map(|p| p.name.clone()));
                            reports::set_identity(&s.device_id, names);
                        }
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    });
            }
            app.manage(Shared {
                gate: gate.clone(),
                ui: std::sync::Mutex::new(ui.clone()),
                ui_path,
                send_queue: std::sync::Mutex::new(Vec::new()),
                send_armed: std::sync::atomic::AtomicBool::new(false),
            });
            gate.hide_fullscreen
                .store(ui.hide_fullscreen, std::sync::atomic::Ordering::Relaxed);
            gate.monitor_cursor
                .store(ui.monitor_cursor, std::sync::atomic::Ordering::Relaxed);

            create_main_window(app)?;
            ghost::spawn(app.handle().clone());

            // Island: borderless, transparent, never takes focus.
            if let Some(win) = island::window(app.handle()) {
                island::make_non_activating(&win);
                island::apply_geometry(app.handle(), gate.pref(), true);
                if ui.enabled {
                    let _ = win.show();
                }
                log_line(
                    app.handle(),
                    &format!(
                        "island window ok: pos={:?} size={:?} visible={:?}",
                        win.outer_position().ok(),
                        win.outer_size().ok(),
                        win.is_visible().ok()
                    ),
                );
            } else {
                log_line(app.handle(), "island window MISSING");
            }
            gate.set_active(ui.enabled);
            island::spawn_cursor_poll(app.handle().clone(), gate);

            // Ctrl+Alt+Z opens / closes the control bar, Ctrl+Alt+U the transfer bar.
            for (key, label) in [(Code::KeyZ, "Z"), (Code::KeyU, "U")] {
                if let Err(e) = app.global_shortcut().register(Shortcut::new(
                    Some(Modifiers::CONTROL | Modifiers::ALT),
                    key,
                )) {
                    log_line(
                        app.handle(),
                        &format!("hotkey Ctrl+Alt+{label} not registered: {e}"),
                    );
                }
            }

            // Keep the right-click entry pointing at this copy of the program.
            if shellmenu::is_on() {
                let _ = shellmenu::set(true);
            }
            // Started by "Send with Tandem" itself: send once the connection is up.
            queue_sends(
                app.handle(),
                send_args(std::env::args()),
                Duration::from_secs(12),
            );

            // Tray.
            let open = MenuItem::with_id(app, "open", "Pair / settings…", true, None::<&str>)?;
            let bar = MenuItem::with_id(app, "bar", "Top bar  (Ctrl+Alt+Z)", true, None::<&str>)?;
            let pill = MenuItem::with_id(
                app,
                "pill",
                "Transfer bar  (Ctrl+Alt+U)",
                true,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&bar, &pill, &open, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("CubeCursor")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "open" => show_main(app),
                    "bar" => bar_hotkey(app, "bar"),
                    "pill" => bar_hotkey(app, "pill"),
                    "quit" => {
                        app.state::<AppState>().node.disconnect();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        bar_hotkey(tray.app_handle(), "bar");
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // First run: show the pairing window unless started by "Start with Windows".
            if !std::env::args().any(|a| a == "--hidden") {
                let paired = app.state::<AppState>().node.snapshot().paired.is_empty();
                if paired {
                    show_main(app.handle());
                }
            }
            Ok(())
        })
        // Closing the pairing window keeps Tandem running (the hooks must stay alive).
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            platform_info,
            open_mac_privacy,
            sound_register,
            sound_play,
            snapshot,
            license_status,
            license_activate,
            license_trial,
            license_release,
            connect,
            connect_known,
            disconnect,
            set_side,
            forget,
            send_files,
            pick_and_send,
            set_clipboard_sync,
            set_name,
            new_code,
            engage,
            open_downloads,
            autostart_get,
            autostart_set,
            stage_begin,
            stage_chunk,
            stage_finish,
            log_js,
            set_game_mode,
            set_drag_files,
            set_move_apps,
            ghost_show,
            set_switch_delay,
            ui_get,
            ui_set,
            set_collapsed,
            set_island_rect,
            end_reveal,
            shell_menu_get,
            shell_menu_set,
            elevation_state,
            restart_elevated,
            elevated_autostart_set,
            reposition,
            open_main_window,
            quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building CubeCursor")
        .run(|_app, event| {
            // Never leave the pointer hidden behind us.
            if let tauri::RunEvent::Exit = event {
                #[cfg(windows)]
                tandem_core::win::restore_cursors();
            }
        });
}
