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
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;

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

// ------------------------------------------------------------------ shell

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
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

            // Tray.
            let open = MenuItem::with_id(app, "open", "Open Tandem", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("Tandem")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "open" => show_main(app),
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
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // Started by "Start with Windows": stay in the tray.
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        // Closing the window keeps Tandem running in the tray (the hooks must stay alive).
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tandem");
}
