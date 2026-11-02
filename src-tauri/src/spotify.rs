//! Spotify: Up next, liking the song, adding it to a playlist. Windows' media controls
//! know none of these, so this uses Spotify's own Web API, signed in with the person's
//! own Spotify app (their Client ID; PKCE, so no secret is needed). The sign-in comes back
//! to CubeCursor's small local server (ai.rs) at /spotify/callback.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest;
use tauri::{AppHandle, Emitter};

pub const REDIRECT: &str = "http://127.0.0.1:47821/spotify/callback";
const SCOPES: &str = "user-read-playback-state user-read-currently-playing user-library-read user-library-modify playlist-read-private playlist-read-collaborative playlist-modify-private playlist-modify-public";

struct Sp {
    app: AppHandle,
    file: PathBuf,
    /// The sign-in waiting for its answer: (state, code verifier).
    pending: Mutex<Option<(String, String)>>,
    token: Mutex<Option<(String, Instant)>>,
    me: Mutex<Option<String>>,
}
static SP: std::sync::OnceLock<Sp> = std::sync::OnceLock::new();
fn sp() -> Result<&'static Sp, String> {
    SP.get().ok_or_else(|| "CubeCursor is starting".to_string())
}

pub fn start(app: &AppHandle, cfg_dir: &std::path::Path) {
    let _ = SP.set(Sp { app: app.clone(), file: cfg_dir.join("spotify.json"), pending: Mutex::new(None), token: Mutex::new(None), me: Mutex::new(None) });
}

fn client_id() -> Option<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(&sp().ok()?.file).ok()?).ok()?;
    v["client_id"].as_str().map(String::from).filter(|s| !s.is_empty())
}

// The refresh token: Windows' credential store (a file elsewhere).
#[cfg(windows)]
fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new("CubeCursor", "spotify-refresh-token").ok()
}
fn refresh_get() -> Option<String> {
    #[cfg(windows)]
    return entry()?.get_password().ok().filter(|t| !t.is_empty());
    #[cfg(not(windows))]
    return std::fs::read_to_string(sp().ok()?.file.with_extension("token")).ok().filter(|t| !t.is_empty());
}
fn refresh_set(t: Option<&str>) {
    #[cfg(windows)]
    if let Some(e) = entry() {
        match t {
            Some(t) => {
                let _ = e.set_password(t);
            }
            None => {
                let _ = e.delete_credential();
            }
        }
    }
    #[cfg(not(windows))]
    if let Ok(s) = sp() {
        let p = s.file.with_extension("token");
        match t {
            Some(t) => {
                let _ = std::fs::write(p, t);
            }
            None => {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}
/// 64 unguessable bytes (each `RandomState` is seeded from the OS).
fn random() -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::with_capacity(64);
    for i in 0..8u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    sha2::Sha512::digest(&out).to_vec()
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build()
}

#[tauri::command]
pub fn spotify_status() -> Value {
    json!({ "connected": refresh_get().is_some(), "clientId": client_id().unwrap_or_default(), "redirect": REDIRECT })
}

/// Opens Spotify's "Allow CubeCursor?" page in the browser.
#[tauri::command]
pub fn spotify_connect(client_id: String) -> Result<(), String> {
    let s = sp()?;
    let id = client_id.trim().to_string();
    if id.len() < 16 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("That doesn't look like a Client ID (32 letters and numbers, from your app's page on developer.spotify.com).".into());
    }
    std::fs::write(&s.file, json!({ "client_id": id }).to_string()).map_err(|e| e.to_string())?;
    let verifier = b64url(&random());
    let state = b64url(&random()[..16]);
    let challenge = b64url(&sha2::Sha256::digest(verifier.as_bytes()));
    *s.pending.lock().unwrap() = Some((state.clone(), verifier));
    let url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
        enc(&id), enc(SCOPES), enc(REDIRECT), enc(&state), enc(&challenge)
    );
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&url).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// The Spotify developer dashboard, in the browser.
#[tauri::command]
pub fn spotify_connect_page() -> Result<(), String> {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", "https://developer.spotify.com/dashboard"]).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("open").arg("https://developer.spotify.com/dashboard").spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn spotify_disconnect() {
    refresh_set(None);
    if let Ok(s) = sp() {
        *s.token.lock().unwrap() = None;
        *s.me.lock().unwrap() = None;
        let _ = s.app.emit("spotify", false);
    }
}

fn token_reply(v: &Value) -> Result<String, String> {
    let s = sp()?;
    let access = v["access_token"].as_str().ok_or("Spotify didn't send a token")?.to_string();
    if let Some(r) = v["refresh_token"].as_str() {
        refresh_set(Some(r));
    }
    let secs = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
    *s.token.lock().unwrap() = Some((access.clone(), Instant::now() + Duration::from_secs(secs)));
    Ok(access)
}

fn post_token(form: &[(&str, &str)]) -> Result<Value, String> {
    match agent().post("https://accounts.spotify.com/api/token").send_form(form) {
        Ok(r) => r.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(_, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            Err(v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("Spotify said no").to_string())
        }
        Err(e) => Err(format!("Couldn't reach Spotify: {e}")),
    }
}

/// The browser comes back here after "Allow". Returns the page to show.
pub fn callback(query: &str) -> String {
    let get = |k: &str| query.split('&').find_map(|p| p.strip_prefix(&format!("{k}="))).map(|v| v.replace("%2F", "/").replace("%3D", "="));
    let page = |title: &str, text: &str| format!(
        "<!doctype html><meta charset=utf-8><title>CubeCursor</title><body style=\"font:16px system-ui;display:grid;place-items:center;height:90vh;text-align:center;background:#0b0d12;color:#fff\"><div><div style=\"font-size:42px\">🎧</div><h2>{title}</h2><p style=\"color:#9aa\">{text}</p></div>"
    );
    let res = (|| -> Result<(), String> {
        let s = sp()?;
        if let Some(e) = get("error") {
            return Err(if e == "access_denied" { "You pressed Cancel.".into() } else { e });
        }
        let (state, verifier) = s.pending.lock().unwrap().take().ok_or("This sign-in has expired. Press Connect again.")?;
        if get("state").as_deref() != Some(state.as_str()) {
            return Err("This sign-in doesn't match. Press Connect again.".into());
        }
        let code = get("code").ok_or("Spotify didn't send a code")?;
        let id = client_id().ok_or("No Client ID")?;
        let v = post_token(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT), ("client_id", &id), ("code_verifier", &verifier)])?;
        token_reply(&v)?;
        let _ = s.app.emit("spotify", true);
        Ok(())
    })();
    match res {
        Ok(()) => page("Spotify is connected", "You can close this tab. The like button and Up next are in CubeCursor's bar now."),
        Err(e) => page("Couldn't connect Spotify", &e),
    }
}

fn token() -> Result<String, String> {
    let s = sp()?;
    if let Some((t, until)) = s.token.lock().unwrap().clone() {
        if Instant::now() < until {
            return Ok(t);
        }
    }
    let r = refresh_get().ok_or("Spotify isn't connected")?;
    let id = client_id().ok_or("Spotify isn't connected")?;
    match post_token(&[("grant_type", "refresh_token"), ("refresh_token", &r), ("client_id", &id)]) {
        Ok(v) => token_reply(&v),
        Err(e) => {
            if e.contains("revoked") || e.contains("invalid") {
                refresh_set(None);
                let _ = s.app.emit("spotify", false);
            }
            Err(e)
        }
    }
}

/// One Web API call; None for "204 No Content".
fn api(method: &str, path: &str, body: Option<Value>) -> Result<Option<Value>, String> {
    let t = token()?;
    let req = agent().request(method, &format!("https://api.spotify.com/v1{path}")).set("Authorization", &format!("Bearer {t}"));
    let res = match body {
        Some(b) => req.send_json(b),
        None if method == "GET" => req.call(),
        None => req.set("Content-Length", "0").send_bytes(&[]),
    };
    match res {
        Ok(r) if r.status() == 204 => Ok(None),
        Ok(r) => {
            let text = r.into_string().map_err(|e| e.to_string())?;
            Ok(if text.trim().is_empty() { None } else { serde_json::from_str(&text).ok() })
        }
        Err(ureq::Error::Status(code, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            let m = v["error"]["message"].as_str().unwrap_or("").to_string();
            Err(match code {
                401 => "Spotify signed CubeCursor out. Connect it again in Settings.".into(),
                403 => format!("Spotify won't allow that{}", if m.is_empty() { String::new() } else { format!(": {m}") }),
                429 => "Spotify asks to slow down. Try again in a moment.".into(),
                _ => format!("Spotify: {code} {m}"),
            })
        }
        Err(e) => Err(format!("Couldn't reach Spotify: {e}")),
    }
}

fn artists(t: &Value) -> String {
    t["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

/// The song now, whether it's in your Liked Songs, and the next three in the queue.
#[tauri::command]
pub async fn spotify_now() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(q) = api("GET", "/me/player/queue", None)? else { return Ok(Value::Null) };
        let cur = &q["currently_playing"];
        let Some(id) = cur["id"].as_str() else { return Ok(Value::Null) };
        let liked = api("GET", &format!("/me/tracks/contains?ids={id}"), None)?
            .and_then(|v| v.get(0).and_then(|b| b.as_bool()))
            .unwrap_or(false);
        let next: Vec<Value> = q["queue"].as_array().map(|a| a.iter().take(3).map(|t| json!({ "name": t["name"], "artist": artists(t) })).collect()).unwrap_or_default();
        Ok(json!({ "id": id, "name": cur["name"], "artist": artists(cur), "liked": liked, "next": next }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn spotify_like(id: String, on: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || api(if on { "PUT" } else { "DELETE" }, &format!("/me/tracks?ids={}", enc(&id)), None).map(|_| ()))
        .await
        .map_err(|e| e.to_string())?
}

/// Your own playlists (and ones you can add to).
#[tauri::command]
pub async fn spotify_playlists() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let s = sp()?;
        let me = match s.me.lock().unwrap().clone() {
            Some(m) => m,
            None => {
                let v = api("GET", "/me", None)?.unwrap_or(Value::Null);
                let m = v["id"].as_str().unwrap_or("").to_string();
                *s.me.lock().unwrap() = Some(m.clone());
                m
            }
        };
        let v = api("GET", "/me/playlists?limit=50", None)?.unwrap_or(Value::Null);
        let list: Vec<Value> = v["items"].as_array().map(|a| a.iter()
            .filter(|p| p["owner"]["id"].as_str() == Some(me.as_str()) || p["collaborative"].as_bool() == Some(true))
            .map(|p| json!({ "id": p["id"], "name": p["name"] }))
            .collect()).unwrap_or_default();
        Ok(Value::Array(list))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn spotify_add(playlist: String, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let body = json!({ "uris": [format!("spotify:track:{id}")] });
        // Newer name first, the older one if this account still has that.
        api("POST", &format!("/playlists/{}/items", enc(&playlist)), Some(body.clone()))
            .or_else(|_| api("POST", &format!("/playlists/{}/tracks", enc(&playlist)), Some(body)))
            .map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
}

