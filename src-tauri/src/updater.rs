//! Automatic updates: asks cubecursor.shop now and then whether there is a newer
//! version, tells the bar and Settings when there is, and installs it when the person
//! says so: download, check it is the very file the website signed, run the installer,
//! and start again.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;
use tandem_core::update::{self, Release};
use tauri::{AppHandle, Emitter};

const ENDPOINT: &str = "https://cubecursor.shop/api/app/update";
/// First look a little after start, then every few hours.
const FIRST_AFTER: Duration = Duration::from_secs(45);
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Installers bigger than this aren't ours.
const MAX_BYTES: u64 = 300 * 1024 * 1024;

static READY: Mutex<Option<Release>> = Mutex::new(None);

fn os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// What the bar and Settings show.
#[derive(Serialize, Clone)]
pub struct UpdateView {
    pub current: String,
    pub version: Option<String>,
    pub notes: String,
}

fn view() -> UpdateView {
    let r = READY.lock().unwrap().clone();
    UpdateView {
        current: env!("CARGO_PKG_VERSION").into(),
        version: r.as_ref().map(|r| r.version.clone()),
        notes: r.map(|r| r.notes).unwrap_or_default(),
    }
}

/// Asks the website once. A newer, properly signed version is remembered and announced.
fn check(app: &AppHandle) -> Result<(), String> {
    let current = env!("CARGO_PKG_VERSION");
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).build();
    let resp = agent
        .get(ENDPOINT)
        .query("os", os())
        .query("v", current)
        .call()
        .map_err(|e| e.to_string())?;
    let body: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    if body.get("available").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(());
    }
    let r: Release = serde_json::from_value(body).map_err(|e| e.to_string())?;
    if !update::newer(&r.version, current) {
        return Ok(());
    }
    if !update::verify(os(), &r, &tandem_core::license::PUBLIC_KEY) {
        log::warn!("update: {} is offered but its signature doesn't check out: ignored", r.version);
        return Ok(());
    }
    let fresh = READY.lock().unwrap().as_ref().map(|x| x.version.clone()) != Some(r.version.clone());
    if fresh {
        log::info!("update: {} is ready", r.version);
        *READY.lock().unwrap() = Some(r);
        let _ = app.emit("update", view());
    }
    Ok(())
}

/// Looks for updates in the background, for as long as the app runs.
pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("cubecursor-update".into())
        .spawn(move || {
            std::thread::sleep(FIRST_AFTER);
            loop {
                if let Err(e) = check(&app) {
                    log::info!("update: couldn't check ({e})");
                }
                std::thread::sleep(EVERY);
            }
        })
        .ok();
}

#[tauri::command]
pub fn update_status() -> UpdateView {
    view()
}

/// Settings › "Check now".
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<UpdateView, String> {
    tauri::async_runtime::spawn_blocking(move || check(&app).map(|_| view()))
        .await
        .map_err(|e| e.to_string())?
}

/// Downloads the installer, checks it, and installs it (the app closes and starts again).
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let Some(r) = READY.lock().unwrap().clone() else {
        return Err("There's no update right now".into());
    };
    let file = tauri::async_runtime::spawn_blocking(move || download(&r))
        .await
        .map_err(|e| e.to_string())??;
    install(&app, &file)
}

fn download(r: &Release) -> Result<std::path::PathBuf, String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(600)).build();
    let resp = agent.get(&r.url).call().map_err(|_| "Couldn't download the update. Are you online?".to_string())?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The download stopped halfway. Try again.".to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("That download is far too big to be CubeCursor".into());
    }
    let sum: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if sum != r.sha256 {
        log::warn!("update: the download doesn't match the signed checksum: not installed");
        return Err("The download wasn't the right file, so it wasn't installed".into());
    }
    let name = if cfg!(windows) {
        format!("CubeCursor-{}-setup.exe", r.version)
    } else {
        format!("CubeCursor-{}.dmg", r.version)
    };
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(windows)]
fn install(app: &AppHandle, file: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Wait for this app to close, install quietly (same place, this user), start again.
    let script = format!(
        "ping -n 3 127.0.0.1 >nul & \"{}\" /S & start \"\" \"{}\"",
        file.display(),
        exe.display()
    );
    std::process::Command::new("cmd")
        .args(["/C", &script])
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| e.to_string())?;
    log::info!("update: installing and restarting");
    app.exit(0);
    Ok(())
}

#[cfg(target_os = "macos")]
fn install(_app: &AppHandle, file: &std::path::Path) -> Result<(), String> {
    // A disk image: macOS opens it, the person drags CubeCursor into Applications.
    std::process::Command::new("open")
        .arg(file)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn install(_app: &AppHandle, _file: &std::path::Path) -> Result<(), String> {
    Err("Updates install by themselves on Windows and macOS".into())
}
