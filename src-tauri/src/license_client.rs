//! Client for CubeCursor's signed licence-pass API.
//! The server signs passes; the app only stores the user's key and the opaque signed pass.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, time::Duration};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::process::Command;
use tandem_core::{license, node::Node};

const API: &str = "https://cubecursor.shop/api/license";

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SavedLicense {
    #[serde(default)] pub key: Option<String>,
    #[serde(default)] pub pass: Option<String>,
    #[serde(default)] pub exp: Option<u64>,
    #[serde(default)] pub ends: Option<u64>,
    #[serde(default)] pub kind: Option<String>,
    #[serde(default)] pub last_error: Option<String>,
}
#[derive(Serialize)] struct LeaseRequest<'a> { key: &'a str, hw: &'a str, dev: &'a str, name: &'a str, #[serde(skip_serializing_if="Option::is_none")] takeover: Option<bool> }
#[derive(Serialize)] struct TrialRequest<'a> { hw: &'a str, dev: &'a str }
#[derive(Serialize)] struct ReleaseRequest<'a> { key: &'a str, hw: &'a str }
#[derive(Deserialize)] struct ApiReply { ok: bool, #[serde(default)] pass: Option<String>, #[serde(default)] exp: Option<u64>, #[serde(default)] ends: Option<u64>, #[serde(default)] reason: Option<String>, #[serde(default)] on: Option<String> }
#[derive(Serialize)] pub struct LicenseView { pub active: bool, pub kind: Option<String>, pub exp: Option<u64>, pub ends: Option<u64>, pub has_key: bool, pub message: Option<String> }

fn read(path: &Path) -> SavedLicense { std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default() }
fn write(path: &Path, value: &SavedLicense) -> Result<(), String> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)); }
    Ok(())
}

/// Stable OS machine identifier, hashed so raw machine identifiers never leave the app.
fn machine_identity() -> String {
    #[cfg(target_os="windows")] let raw = { use std::os::windows::process::CommandExt; let mut c = Command::new("reg"); c.creation_flags(0x0800_0000); c }.args(["query", "HKLM\\SOFTWARE\\Microsoft\\Cryptography", "/v", "MachineGuid"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    #[cfg(target_os="macos")] let raw = Command::new("ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    #[cfg(target_os="linux")] let raw = std::fs::read_to_string("/etc/machine-id").or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id")).unwrap_or_default();
    let source = if raw.trim().is_empty() { format!("{}-{}", std::env::var("COMPUTERNAME").unwrap_or_default(), std::env::var("USER").unwrap_or_default()) } else { raw };
    let digest = Sha256::digest(source.trim().as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}
fn dev_id(node: &Node) -> String { node.snapshot().device_id }
fn post<T: Serialize>(url: &str, body: &T) -> Result<ApiReply, String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(8)).timeout(Duration::from_secs(20)).build();
    let response = agent.post(url).set("Content-Type", "application/json").set("User-Agent", concat!("CubeCursor/", env!("CARGO_PKG_VERSION"))).send_json(body);
    match response {
        Ok(r) => r.into_json::<ApiReply>().map_err(|e| format!("Invalid server reply: {e}")),
        Err(ureq::Error::Status(_, r)) => r.into_json::<ApiReply>().map_err(|e| format!("Licence server returned an error ({e})")),
        Err(ureq::Error::Transport(e)) => Err(format!("Could not reach the licence server: {e}")),
    }
}
fn apply_reply(path: &Path, saved: &mut SavedLicense, reply: ApiReply, node: &Node) -> Result<LicenseView, String> {
    if !reply.ok {
        let reason = match reply.reason.as_deref() {
            Some("unknown_key") => "That licence key wasn't recognised.".to_string(),
            Some("revoked") => "This licence has been deactivated.".to_string(),
            Some("in_use") => format!("This licence is currently in use on {}. Choose Move licence to move it here.", reply.on.unwrap_or_else(|| "another computer".into())),
            Some("too_many_moves") => "This licence has reached its daily move limit. Try again later.".into(),
            Some("trial_over") => "The 14-day trial has ended. Enter a licence key to continue.".into(),
            _ => "The licence server couldn't complete that request.".into(),
        };
        saved.last_error = Some(reason.clone()); let _ = write(path, saved);
        return Err(reason);
    }
    let token = reply.pass.ok_or_else(|| "The licence server didn't return a pass.".to_string())?;
    let pass = license::verify(&token, &license::PUBLIC_KEY, license::now()).map_err(|_| "The signed pass could not be verified. Check that the website signing key matches the app's public key.".to_string())?;
    if pass.dev != dev_id(node) || pass.hw != machine_identity() { return Err("The signed pass is for a different computer. Please activate it on this PC.".into()); }
    saved.pass = Some(token.clone()); saved.exp = reply.exp.or(Some(pass.exp)); saved.ends = reply.ends; saved.kind = Some(pass.kind); saved.last_error = None;
    write(path, saved)?; node.set_license_pass(Some(token));
    Ok(view(saved))
}
fn view(s: &SavedLicense) -> LicenseView { LicenseView { active: s.pass.as_ref().is_some_and(|p| s.exp.is_some_and(|e| e > license::now()) && license::verify(p, &license::PUBLIC_KEY, license::now()).is_ok()), kind: s.kind.clone(), exp: s.exp, ends: s.ends, has_key: s.key.is_some(), message: s.last_error.clone() } }

pub fn status(path: &Path) -> LicenseView { view(&read(path)) }
pub fn activate(path: &Path, node: &Node, key: String, takeover: bool) -> Result<LicenseView, String> {
    let key = key.trim().to_ascii_uppercase(); if key.is_empty() { return Err("Enter your licence key.".into()); }
    let hw = machine_identity(); let dev = dev_id(node); let name = node.snapshot().device_name;
    let reply = post(&format!("{API}/lease"), &LeaseRequest { key: &key, hw: &hw, dev: &dev, name: &name, takeover: takeover.then_some(true) })?;
    let mut saved = read(path); saved.key = Some(key);
    apply_reply(path, &mut saved, reply, node)
}
pub fn start_trial(path: &Path, node: &Node) -> Result<LicenseView, String> {
    let hw = machine_identity(); let dev = dev_id(node);
    let reply = post(&format!("{API}/trial"), &TrialRequest { hw: &hw, dev: &dev })?;
    let mut saved = read(path); saved.key = None;
    apply_reply(path, &mut saved, reply, node)
}
pub fn release(path: &Path, node: &Node) -> Result<LicenseView, String> {
    let mut saved = read(path);
    if let Some(key) = saved.key.as_deref() { let _ = post(&format!("{API}/release"), &ReleaseRequest { key, hw: &machine_identity() }); }
    saved.key = None; saved.pass = None; saved.exp = None; saved.ends = None; saved.kind = None; saved.last_error = None;
    write(path, &saved)?; node.set_license_pass(None); Ok(view(&saved))
}
/// Restore a cached pass immediately, then renew once an hour. Network outages don't erase a still-valid pass.
pub fn start_renewal(path: PathBuf, node: Node) {
    let mut saved = read(&path);
    if let Some(token) = saved.pass.clone() {
        if let Ok(pass) = license::verify(&token, &license::PUBLIC_KEY, license::now()) {
            if pass.dev == dev_id(&node) && pass.hw == machine_identity() {
                node.set_license_pass(Some(token));
            } else {
                saved.pass = None; saved.exp = None; saved.last_error = Some("The saved pass belongs to a different computer. Please activate this PC.".into());
                let _ = write(&path, &saved);
            }
        }
    }
    std::thread::Builder::new().name("license-renewal".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3600));
        let mut saved = read(&path);
        let result = if let Some(key) = saved.key.clone() {
            let hw = machine_identity(); let dev = dev_id(&node); let name = node.snapshot().device_name;
            post(&format!("{API}/lease"), &LeaseRequest { key: &key, hw: &hw, dev: &dev, name: &name, takeover: None }).and_then(|r| apply_reply(&path, &mut saved, r, &node).map(|_| ()))
        } else {
            let hw = machine_identity(); let dev = dev_id(&node);
            post(&format!("{API}/trial"), &TrialRequest { hw: &hw, dev: &dev }).and_then(|r| apply_reply(&path, &mut saved, r, &node).map(|_| ()))
        };
        if let Err(e) = result { saved.last_error = Some(e); let _ = write(&path, &saved); }
    }).ok();
}
