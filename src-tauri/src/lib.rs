//! Tandem desktop shell: a tray app around `tandem-core`.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tandem_core::node::*;
use tandem_core::proto::{Edge, DEFAULT_TCP_PORT, DISCOVERY_PORT};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri::{WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;

mod island;
use island::{IslandRect, PollGate};

struct Shared {
    gate: Arc<PollGate>,
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

#[tauri::command]
fn disconnect(st: State<'_, AppState>) {
    st.node.disconnect();
}

#[tauri::command]
fn set_side(st: State<'_, AppState>, peer_id: String, side: String) -> R {
    let side = match side.as_str() {
        "left" => Edge::Left,
        "right" => Edge::Right,
        "top" => Edge::Top,
        "bottom" => Edge::Bottom,
        _ => return Err("bad side".into()),
    };
    st.node.set_side(peer_id, side);
    Ok(())
}

#[tauri::command]
fn forget(st: State<'_, AppState>, id: String) {
    st.node.forget(id);
}

#[tauri::command]
fn send_files(st: State<'_, AppState>, paths: Vec<String>) {
    st.node
        .send_files(paths.into_iter().map(PathBuf::from).collect());
}

#[tauri::command]
async fn pick_and_send(app: AppHandle) -> R {
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
            app.state::<AppState>().node.send_files(paths);
        }
    }
    Ok(())
}

#[tauri::command]
fn set_clipboard_sync(st: State<'_, AppState>, on: bool) {
    st.node.set_clipboard_sync(on);
}

#[tauri::command]
fn set_name(st: State<'_, AppState>, name: String) {
    st.node.set_name(name);
}

#[tauri::command]
fn new_code(st: State<'_, AppState>) {
    st.node.new_pairing_code();
}

#[tauri::command]
fn engage(st: State<'_, AppState>) {
    st.node.engage();
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
    if s.len() % 2 != 0 {
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
fn stage_finish(st: State<'_, AppState>, id: String, names: Vec<String>) -> R {
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
    st.node.send_files(paths);
    Ok(())
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

// ------------------------------------------------------------------ island

const PREF: &str = "primary";

#[tauri::command]
fn set_collapsed(app: AppHandle, sh: State<'_, Shared>, collapsed: bool) {
    sh.gate
        .collapsed
        .store(collapsed, std::sync::atomic::Ordering::Relaxed);
    island::apply_geometry(&app, PREF, collapsed);
    sh.gate.forget_ignore_state();
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

#[tauri::command]
fn reposition(app: AppHandle, sh: State<'_, Shared>) {
    let c = sh.gate.collapsed.load(std::sync::atomic::Ordering::Relaxed);
    island::apply_geometry(&app, PREF, c);
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

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// The pairing / settings window: created hidden, hides instead of closing.
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let b = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("settings.html".into()))
        .title("Tandem")
        .inner_size(460.0, 780.0)
        .min_inner_size(400.0, 560.0)
        .center()
        .resizable(true)
        .visible(false)
        .disable_drag_drop_handler();
    b.build()?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .setup(|app| {
            let cfg_dir = app.path().app_config_dir()?;
            let download_dir = app
                .path()
                .download_dir()
                .unwrap_or_else(|_| cfg_dir.clone())
                .join("Tandem");
            let cfg = NodeConfig {
                tcp_port: DEFAULT_TCP_PORT,
                udp_port: DISCOVERY_PORT,
                temp_dir: std::env::temp_dir().join("Tandem"),
                download_dir: download_dir.clone(),
                discovery: true,
                default_name: std::env::var("COMPUTERNAME")
                    .unwrap_or_else(|_| "My computer".into()),
            };
            let node = tauri::async_runtime::block_on(Node::start(
                cfg,
                tandem_core::default_platform(),
                Arc::new(AppStore {
                    path: cfg_dir.join("state.json"),
                }),
            ))?;

            // Push state to the UI, at most ~12 times a second.
            let mut rx = node.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    let s = rx.borrow_and_update().clone();
                    let _ = handle.emit("snapshot", &s);
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
            });
            app.manage(AppState { node, download_dir });

            let _ = std::fs::remove_dir_all(stage_root());
            let gate = Arc::new(PollGate::new());
            app.manage(Shared { gate: gate.clone() });

            create_main_window(app)?;

            // Island: borderless, transparent, never takes focus.
            if let Some(win) = island::window(app.handle()) {
                island::make_non_activating(&win);
                island::apply_geometry(app.handle(), PREF, true);
                let _ = win.show();
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
            gate.set_active(true);
            island::spawn_cursor_poll(app.handle().clone(), gate, PREF);

            // Tray.
            let open = MenuItem::with_id(app, "open", "Pair / settings…", true, None::<&str>)?;
            let bar = MenuItem::with_id(app, "bar", "Show the top bar", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&bar, &open, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("Tandem")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "open" => show_main(app),
                    "bar" => {
                        let _ = app.emit_to(island::WINDOW_LABEL, "tray", "open");
                    }
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
                        let _ = tray
                            .app_handle()
                            .emit_to(island::WINDOW_LABEL, "tray", "open");
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
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
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
            set_collapsed,
            set_island_rect,
            reposition,
            open_main_window,
            quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tandem");
}
