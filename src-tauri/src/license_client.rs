//! Client for CubeCursor's signed licence-pass API.
//!
//! How licensing works: a key is redeemed to a CubeCursor account on the website, and the
//! app signs in with that account (the browser asks the person to allow this PC). The
//! website then signs a pass for this computer, which the app renews every hour with a
//! token only this PC holds. The licence runs on one computer at a time: signing in on
//! another moves it there, and this one hears "moved" at its next renewal and stops.
//! Without an account, a computer gets one 14-day trial. The app stores the token and the
//! opaque signed pass, never a password.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::process::Command;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tandem_core::{license, node::Node};

const SITE: &str = "https://cubecursor.shop";
const API: &str = "https://cubecursor.shop/api/license";
const APP_API: &str = "https://cubecursor.shop/api/app";

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SavedLicense {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub pass: Option<String>,
    #[serde(default)]
    pub exp: Option<u64>,
    #[serde(default)]
    pub ends: Option<u64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// The sign-in token (only works on this computer), from signing in.
    #[serde(default)]
    pub token: Option<String>,
    /// The account signed in with.
    #[serde(default)]
    pub email: Option<String>,
    /// Signed in, but another computer took the licence: its name.
    #[serde(default)]
    pub moved_to: Option<String>,
    /// The latest time this app has seen (a clock set back doesn't bring a pass back).
    #[serde(default)]
    pub seen: Option<u64>,
}
#[derive(Serialize)]
struct LeaseRequest<'a> {
    key: &'a str,
    hw: &'a str,
    dev: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    takeover: Option<bool>,
}
#[derive(Serialize)]
struct TrialRequest<'a> {
    hw: &'a str,
    dev: &'a str,
}
#[derive(Serialize)]
struct ReleaseRequest<'a> {
    key: &'a str,
    hw: &'a str,
}
#[derive(Serialize)]
struct LinkStart<'a> {
    hw: &'a str,
    dev: &'a str,
    name: &'a str,
}
#[derive(Serialize)]
struct LinkPoll<'a> {
    poll: &'a str,
}
#[derive(Serialize)]
struct Renew<'a> {
    token: &'a str,
    hw: &'a str,
    dev: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    takeover: Option<bool>,
}
#[derive(Serialize)]
struct SignOut<'a> {
    token: &'a str,
    hw: &'a str,
}
#[derive(Deserialize, Default)]
struct ApiReply {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    poll: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    pass: Option<String>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    ends: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    on: Option<String>,
}
#[derive(Serialize)]
pub struct LicenseView {
    pub active: bool,
    pub kind: Option<String>,
    pub exp: Option<u64>,
    pub ends: Option<u64>,
    pub has_key: bool,
    pub message: Option<String>,
    /// Signed in with this account.
    pub email: Option<String>,
    /// Another computer has the licence now (its name).
    pub moved_to: Option<String>,
}

fn read(path: &Path) -> SavedLicense {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn write(path: &Path, value: &SavedLicense) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Stable OS machine identifier, hashed so raw machine identifiers never leave the app.
fn machine_identity() -> String {
    #[cfg(target_os = "windows")]
    let raw = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new(r"C:\Windows\System32\reg.exe");
        c.creation_flags(0x0800_0000);
        c
    }
    .args([
        "query",
        "HKLM\\SOFTWARE\\Microsoft\\Cryptography",
        "/v",
        "MachineGuid",
    ])
    .output()
    .ok()
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();
    #[cfg(target_os = "macos")]
    let raw = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    #[cfg(target_os = "linux")]
    let raw = std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .unwrap_or_default();
    let source = if raw.trim().is_empty() {
        format!(
            "{}-{}",
            std::env::var("COMPUTERNAME").unwrap_or_default(),
            std::env::var("USER").unwrap_or_default()
        )
    } else {
        raw
    };
    let digest = Sha256::digest(source.trim().as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}
fn dev_id(node: &Node) -> String {
    node.snapshot().device_id
}
fn post<T: Serialize>(url: &str, body: &T) -> Result<ApiReply, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .build();
    let response = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set(
            "User-Agent",
            concat!("CubeCursor/", env!("CARGO_PKG_VERSION")),
        )
        .send_json(body);
    match response {
        Ok(r) => r
            .into_json::<ApiReply>()
            .map_err(|e| format!("Invalid server reply: {e}")),
        Err(ureq::Error::Status(_, r)) => r
            .into_json::<ApiReply>()
            .map_err(|e| format!("Licence server returned an error ({e})")),
        Err(ureq::Error::Transport(e)) => Err(format!("Could not reach the licence server: {e}")),
    }
}
fn apply_reply(
    path: &Path,
    saved: &mut SavedLicense,
    reply: ApiReply,
    node: &Node,
) -> Result<LicenseView, String> {
    if !reply.ok {
        let reason = match reply.reason.as_deref() {
            Some("unknown_key") => "That licence key wasn't recognised.".to_string(),
            Some("revoked") => "This licence has been deactivated.".to_string(),
            Some("in_use") => format!(
                "This licence is currently in use on {}. Choose Move licence to move it here.",
                reply.on.unwrap_or_else(|| "another computer".into())
            ),
            Some("too_many_moves") => {
                "This licence has reached its daily move limit. Try again later.".into()
            }
            Some("trial_over") => {
                "The 14-day trial on this computer has ended. Sign in with the account your key is redeemed to.".into()
            }
            Some("trial_limit") => {
                "Too many trials were started from this network today. Try again tomorrow, or sign in with your account.".into()
            }
            Some("sign_in_required") => {
                "Keys are redeemed to an account now: redeem yours at cubecursor.shop/dashboard, then choose Sign in.".into()
            }
            Some("no_license") => {
                "This account has no licence yet. Redeem your key at cubecursor.shop/dashboard, then sign in again.".into()
            }
            _ => "The licence server couldn't complete that request.".into(),
        };
        saved.last_error = Some(reason.clone());
        let _ = write(path, saved);
        return Err(reason);
    }
    let token = reply
        .pass
        .ok_or_else(|| "The licence server didn't return a pass.".to_string())?;
    let pass = license::verify(&token, &license::PUBLIC_KEY, license::now()).map_err(|_| "The signed pass could not be verified. Check that the website signing key matches the app's public key.".to_string())?;
    if pass.dev != dev_id(node) || pass.hw != machine_identity() {
        return Err(
            "The signed pass is for a different computer. Please activate it on this PC.".into(),
        );
    }
    saved.pass = Some(token.clone());
    saved.exp = reply.exp.or(Some(pass.exp));
    saved.ends = reply.ends;
    saved.kind = Some(pass.kind);
    saved.last_error = None;
    saved.moved_to = None;
    saved.seen = Some(saved.seen.unwrap_or(0).max(license::now()));
    write(path, saved)?;
    node.set_license_pass(Some(token));
    Ok(view(saved))
}

/// Stops using the licence here (the pass goes; the account stays signed in unless asked).
fn drop_pass(path: &Path, saved: &mut SavedLicense, node: &Node) {
    saved.pass = None;
    saved.exp = None;
    saved.kind = None;
    let _ = write(path, saved);
    node.set_license_pass(None);
}

/// A signed-in renewal (or "Use it here"): the server's answer applied.
fn renew(path: &Path, node: &Node, takeover: bool) -> Result<LicenseView, String> {
    let mut saved = read(path);
    let Some(token) = saved.token.clone() else {
        return Err("Sign in first.".into());
    };
    let hw = machine_identity();
    let dev = dev_id(node);
    let name = node.snapshot().device_name;
    let reply = post(
        &format!("{APP_API}/renew"),
        &Renew {
            token: &token,
            hw: &hw,
            dev: &dev,
            name: &name,
            takeover: takeover.then_some(true),
        },
    )?;
    if !reply.ok {
        match reply.reason.as_deref() {
            Some("moved") => {
                // Another computer is the main now: stop here at once.
                saved.moved_to = Some(reply.on.unwrap_or_else(|| "another computer".into()));
                saved.last_error = None;
                drop_pass(path, &mut saved, node);
                return Ok(view(&saved));
            }
            Some("signed_out") => {
                saved.token = None;
                saved.email = None;
                saved.moved_to = None;
                saved.last_error = Some("This computer was signed out. Sign in again to use your licence here.".into());
                drop_pass(path, &mut saved, node);
                return Ok(view(&saved));
            }
            Some("revoked") => {
                saved.last_error = Some("This licence was refunded or deactivated.".into());
                drop_pass(path, &mut saved, node);
                return Ok(view(&saved));
            }
            _ => {}
        }
    }
    apply_reply(path, &mut saved, reply, node)
}

// ---------------------------------------------------------------- signing in

/// The sign-in in progress: the secret only this app knows, and the page to open.
static SIGNIN: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

#[derive(Serialize)]
pub struct SignInStart {
    pub code: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Serialize)]
pub struct SignInPoll {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<LicenseView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Opens a cubecursor.shop page in the browser.
fn open_browser(url: &str) -> Result<(), String> {
    if !url.starts_with(SITE) {
        return Err("Unexpected sign-in address.".into());
    }
    #[cfg(target_os = "windows")]
    let r = {
        use std::os::windows::process::CommandExt;
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x0800_0000)
            .spawn()
    };
    #[cfg(target_os = "macos")]
    let r = Command::new("open").arg(url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    r.map(|_| ()).map_err(|e| format!("Couldn't open the browser: {e}"))
}

/// "Sign in": asks the website for a code and opens the page where the person allows this PC.
pub fn signin_start(node: &Node) -> Result<SignInStart, String> {
    let hw = machine_identity();
    let dev = dev_id(node);
    let name = node.snapshot().device_name;
    let r = post(&format!("{APP_API}/link/start"), &LinkStart { hw: &hw, dev: &dev, name: &name })?;
    let (Some(code), Some(poll), Some(url)) = (r.code, r.poll, r.url) else {
        return Err("The sign-in service didn't answer properly. Try again in a moment.".into());
    };
    *SIGNIN.lock().unwrap() = Some((poll, url.clone()));
    open_browser(&url)?;
    Ok(SignInStart {
        code,
        interval: r.interval.unwrap_or(2).clamp(1, 10),
        expires_in: r.expires_in.unwrap_or(600),
    })
}

/// Has the person allowed this PC in the browser yet?
pub fn signin_poll(path: &Path, node: &Node) -> Result<SignInPoll, String> {
    let Some((poll, _)) = SIGNIN.lock().unwrap().clone() else {
        return Ok(SignInPoll { status: "expired".into(), view: None, error: None });
    };
    let r = post(&format!("{APP_API}/link/poll"), &LinkPoll { poll: &poll })?;
    let status = r.status.clone().unwrap_or_else(|| "pending".into());
    if status != "done" {
        if status != "pending" {
            *SIGNIN.lock().unwrap() = None;
        }
        return Ok(SignInPoll { status, view: None, error: None });
    }
    *SIGNIN.lock().unwrap() = None;
    let mut saved = read(path);
    if r.ok {
        saved.token = r.token.clone();
        saved.email = r.email.clone();
        saved.key = None;
        return match apply_reply(path, &mut saved, r, node) {
            Ok(v) => Ok(SignInPoll { status, view: Some(v), error: None }),
            Err(e) => Ok(SignInPoll { status, view: Some(view(&saved)), error: Some(e) }),
        };
    }
    let err = apply_reply(path, &mut saved, r, node).err();
    Ok(SignInPoll { status, view: Some(view(&saved)), error: err })
}

pub fn signin_cancel() {
    *SIGNIN.lock().unwrap() = None;
}

/// Opens the sign-in page again (closed by mistake).
pub fn signin_open() -> Result<(), String> {
    let url = SIGNIN.lock().unwrap().as_ref().map(|(_, u)| u.clone());
    match url {
        Some(u) => open_browser(&u),
        None => Err("Start signing in again.".into()),
    }
}

/// "Sign out": this PC stops using the licence and forgets the account.
pub fn signout(path: &Path, node: &Node) -> Result<LicenseView, String> {
    let mut saved = read(path);
    if let Some(token) = saved.token.as_deref() {
        let _ = post(&format!("{APP_API}/signout"), &SignOut { token, hw: &machine_identity() });
    }
    saved.token = None;
    saved.email = None;
    saved.moved_to = None;
    saved.key = None;
    saved.last_error = None;
    drop_pass(path, &mut saved, node);
    Ok(view(&saved))
}

/// "Use it here": takes the licence back from the computer that has it now.
pub fn use_here(path: &Path, node: &Node) -> Result<LicenseView, String> {
    renew(path, node, true)
}
fn view(s: &SavedLicense) -> LicenseView {
    LicenseView {
        active: s.pass.as_ref().is_some_and(|p| {
            s.exp.is_some_and(|e| e > license::now())
                && license::verify(p, &license::PUBLIC_KEY, license::now()).is_ok()
        }),
        email: s.email.clone(),
        moved_to: s.moved_to.clone(),
        kind: s.kind.clone(),
        exp: s.exp,
        ends: s.ends,
        has_key: s.key.is_some(),
        message: s.last_error.clone(),
    }
}

pub fn status(path: &Path) -> LicenseView {
    view(&read(path))
}
pub fn activate(
    path: &Path,
    node: &Node,
    key: String,
    takeover: bool,
) -> Result<LicenseView, String> {
    let key = key.trim().to_ascii_uppercase();
    if key.is_empty() {
        return Err("Enter your licence key.".into());
    }
    let hw = machine_identity();
    let dev = dev_id(node);
    let name = node.snapshot().device_name;
    let reply = post(
        &format!("{API}/lease"),
        &LeaseRequest {
            key: &key,
            hw: &hw,
            dev: &dev,
            name: &name,
            takeover: takeover.then_some(true),
        },
    )?;
    let mut saved = read(path);
    saved.key = Some(key);
    apply_reply(path, &mut saved, reply, node)
}
pub fn start_trial(path: &Path, node: &Node) -> Result<LicenseView, String> {
    let hw = machine_identity();
    let dev = dev_id(node);
    let reply = post(
        &format!("{API}/trial"),
        &TrialRequest { hw: &hw, dev: &dev },
    )?;
    let mut saved = read(path);
    saved.key = None;
    apply_reply(path, &mut saved, reply, node)
}
pub fn release(path: &Path, node: &Node) -> Result<LicenseView, String> {
    let mut saved = read(path);
    if let Some(key) = saved.key.as_deref() {
        let _ = post(
            &format!("{API}/release"),
            &ReleaseRequest {
                key,
                hw: &machine_identity(),
            },
        );
    }
    saved.key = None;
    saved.pass = None;
    saved.exp = None;
    saved.ends = None;
    saved.kind = None;
    saved.last_error = None;
    write(path, &saved)?;
    node.set_license_pass(None);
    Ok(view(&saved))
}
/// Restore a cached pass immediately, then renew every few minutes. Network outages don't erase a
/// still-valid pass, but a pass is short-lived, so a computer the licence has left stops quickly.
pub fn start_renewal(path: PathBuf, node: Node) {
    let mut saved = read(&path);
    // A clock set back behind the latest time seen doesn't revive an expired pass.
    license::set_floor(saved.seen.unwrap_or(0));
    if let Some(token) = saved.pass.clone() {
        if let Ok(pass) = license::verify(&token, &license::PUBLIC_KEY, license::now()) {
            if pass.dev == dev_id(&node) && pass.hw == machine_identity() {
                node.set_license_pass(Some(token));
            } else {
                saved.pass = None;
                saved.exp = None;
                saved.last_error = Some(
                    "The saved pass belongs to a different computer. Please activate this PC."
                        .into(),
                );
                let _ = write(&path, &saved);
            }
        }
    }
    std::thread::Builder::new()
        .name("license-renewal".into())
        .spawn(move || {
            // The first check right after start (a new computer gets its trial before its
            // first connection is checked), then every few minutes so a pass that has moved
            // to another computer lapses here soon after.
            std::thread::sleep(Duration::from_secs(2));
            loop {
            let mut saved = read(&path);
            let now = license::now();
            if saved.seen.is_none_or(|s| now > s) {
                saved.seen = Some(now);
                let _ = write(&path, &saved);
            }
            let result = if saved.token.is_some() {
                renew(&path, &node, false).map(|_| ())
            } else if let Some(key) = saved.key.clone() {
                let hw = machine_identity();
                let dev = dev_id(&node);
                let name = node.snapshot().device_name;
                post(
                    &format!("{API}/lease"),
                    &LeaseRequest {
                        key: &key,
                        hw: &hw,
                        dev: &dev,
                        name: &name,
                        takeover: None,
                    },
                )
                .and_then(|r| apply_reply(&path, &mut saved, r, &node).map(|_| ()))
            } else {
                let hw = machine_identity();
                let dev = dev_id(&node);
                post(
                    &format!("{API}/trial"),
                    &TrialRequest { hw: &hw, dev: &dev },
                )
                .and_then(|r| apply_reply(&path, &mut saved, r, &node).map(|_| ()))
            };
            // Normally renew every 5 minutes; after a network error retry in 1 minute so a
            // brief outage doesn't leave the pass to lapse.
            let wait = if let Err(e) = result {
                let mut saved = read(&path);
                saved.last_error = Some(e);
                let _ = write(&path, &saved);
                60
            } else {
                300
            };
            std::thread::sleep(Duration::from_secs(wait));
            }
        })
        .ok();
}
