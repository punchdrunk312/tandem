//! CubeCursor for AI coding agents (Claude Code, Codex…).
//!
//! - Their hooks post to a small server on this computer (127.0.0.1 only); each event goes
//!   to the bar here and on the other computers, so you see "Working / Needs you / Done"
//!   and can approve a permission from any of them.
//! - The same server speaks MCP, so an agent can use your computers: run a command, read
//!   or write a file, take a screenshot, open a page, send a file.
//! - A usage meter (Claude Code's and Codex's own logs), "Ask Bob" (the Claude API with
//!   your key), an error catcher on the clipboard, and things sent to an AI on the other
//!   computer.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::{island, AppState, Shared};

pub const PORT: u16 = 47821;
/// How long an approval waits for an answer from the bar before Claude asks in the
/// terminal as usual.
const APPROVE_WAIT: Duration = Duration::from_secs(60);
const MARK: &str = "cubecursor-hook";

struct Ai {
    app: AppHandle,
    script: PathBuf,
    /// Approvals waiting for an answer, by event id.
    pending: Mutex<HashMap<String, mpsc::Sender<bool>>>,
    /// MCP calls waiting for the other computer, by call id.
    calls: Mutex<HashMap<String, mpsc::Sender<Value>>>,
}
static AI: OnceLock<Ai> = OnceLock::new();

fn ai() -> &'static Ai {
    AI.get().expect("ai::start ran")
}
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
fn new_id() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{:x}{:x}", now_ms(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}
fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default()
}
fn trunc(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}
fn me() -> (String, String) {
    match ai().app.try_state::<AppState>() {
        Some(st) => {
            let s = st.node.snapshot();
            (s.device_id, s.device_name)
        }
        None => (String::new(), String::new()),
    }
}
fn relay(to: Option<String>, kind: &str, v: &Value) {
    if let Some(st) = ai().app.try_state::<AppState>() {
        st.node.relay(to, kind.into(), v.to_string());
    }
}
fn to_bar(event: &str, v: &Value) {
    let _ = ai().app.emit_to(island::WINDOW_LABEL, event, v);
}
fn opts() -> (bool, bool) {
    ai().app.try_state::<Shared>().map(|sh| {
        let u = sh.ui.lock().unwrap();
        (u.ai_remote, u.ai_sync)
    }).unwrap_or((false, false))
}

pub fn start(app: &AppHandle, cfg_dir: &Path) {
    let script = write_script(cfg_dir);
    let _ = AI.set(Ai { app: app.clone(), script, pending: Mutex::new(HashMap::new()), calls: Mutex::new(HashMap::new()) });
    std::thread::Builder::new().name("ai-server".into()).spawn(serve).ok();
    std::thread::Builder::new().name("ai-clip".into()).spawn(watch_clipboard).ok();
    std::thread::Builder::new().name("ai-instr".into()).spawn(sync_instructions).ok();
    if let Some(st) = app.try_state::<AppState>() {
        let mut rx = st.node.relays();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) => {
                        std::thread::spawn(move || on_relay(m));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
}

// ------------------------------------------------------------------ hook script

fn write_script(dir: &Path) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(windows)]
    {
        let p = dir.join(format!("{MARK}.ps1"));
        let body = format!(
            r#"# Posts a Claude Code / Codex hook event to CubeCursor and prints its answer.
param([string]$src, [string]$ev, [string]$payload)
$ErrorActionPreference = 'SilentlyContinue'
if (-not $payload) {{ $payload = [Console]::In.ReadToEnd() }}
try {{
  $r = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:{PORT}/hook/$src/$ev" -Method Post -Body ([Text.Encoding]::UTF8.GetBytes($payload)) -ContentType 'application/json' -TimeoutSec 70
  [Console]::Out.Write($r.Content)
}} catch {{}}
exit 0
"#
        );
        let _ = std::fs::write(&p, body);
        p
    }
    #[cfg(not(windows))]
    {
        let p = dir.join(format!("{MARK}.sh"));
        let body = format!(
            "#!/bin/sh\n# Posts a Claude Code / Codex hook event to CubeCursor and prints its answer.\nif [ -n \"$3\" ]; then printf '%s' \"$3\"; else cat; fi | curl -s -m 70 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:{PORT}/hook/$1/$2\nexit 0\n"
        );
        let _ = std::fs::write(&p, body);
        p
    }
}

fn hook_cmd(src: &str, ev: &str) -> String {
    let s = ai().script.display().to_string();
    #[cfg(windows)]
    return format!("powershell -NoProfile -ExecutionPolicy Bypass -File \"{s}\" {src} {ev}");
    #[cfg(not(windows))]
    return format!("sh \"{s}\" {src} {ev}");
}

// ------------------------------------------------------------------ connecting the tools

const CLAUDE_EVENTS: [&str; 5] = ["UserPromptSubmit", "Notification", "PermissionRequest", "Stop", "SessionEnd"];

fn read_json(p: &Path) -> Result<Value, String> {
    match std::fs::read(p) {
        Ok(b) if !b.is_empty() => serde_json::from_slice(&b).map_err(|e| format!("{} isn't valid JSON ({e}), so it was left alone", p.display())),
        _ => Ok(json!({})),
    }
}
fn write_safely(p: &Path, text: &str) -> Result<(), String> {
    if p.exists() {
        let mut b = p.as_os_str().to_owned();
        b.push(".cubecursor-backup");
        let _ = std::fs::copy(p, PathBuf::from(b));
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("cubecursor-tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

fn claude_connected() -> bool {
    std::fs::read_to_string(home().join(".claude").join("settings.json")).is_ok_and(|s| s.contains(MARK))
}
fn codex_connected() -> bool {
    std::fs::read_to_string(home().join(".codex").join("config.toml")).is_ok_and(|s| s.contains("# cubecursor"))
}

fn claude_set(on: bool) -> Result<String, String> {
    let sp = home().join(".claude").join("settings.json");
    let mut v = read_json(&sp)?;
    if !v.is_object() {
        v = json!({});
    }
    let hooks = v.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().unwrap();
    // Ours out first (so connecting twice never doubles them).
    for (_, list) in hooks.iter_mut() {
        if let Some(a) = list.as_array_mut() {
            a.retain(|g| !g.to_string().contains(MARK));
        }
    }
    hooks.retain(|_, list| list.as_array().is_none_or(|a| !a.is_empty()));
    if on {
        for ev in CLAUDE_EVENTS {
            let list = hooks.entry(ev).or_insert_with(|| json!([]));
            if let Some(a) = list.as_array_mut() {
                a.push(json!({ "hooks": [{ "type": "command", "command": hook_cmd("claude", ev), "timeout": 70 }] }));
            }
        }
    }
    write_safely(&sp, &serde_json::to_string_pretty(&v).unwrap())?;

    // The MCP server, for every project (user scope).
    let cp = home().join(".claude.json");
    let mut c = read_json(&cp)?;
    if c.is_object() {
        let servers = c.as_object_mut().unwrap().entry("mcpServers").or_insert_with(|| json!({}));
        if let Some(m) = servers.as_object_mut() {
            if on {
                m.insert("cubecursor".into(), json!({ "type": "http", "url": format!("http://127.0.0.1:{PORT}/mcp") }));
            } else {
                m.remove("cubecursor");
            }
        }
        write_safely(&cp, &serde_json::to_string_pretty(&c).unwrap())?;
    }
    Ok(if on { "Claude Code is connected. Restart any Claude Code that's open.".into() } else { "Claude Code is disconnected.".into() })
}

fn codex_set(on: bool) -> Result<String, String> {
    let p = home().join(".codex").join("config.toml");
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let mut lines: Vec<String> = old.lines().filter(|l| !l.contains("# cubecursor")).map(String::from).collect();
    let mut msg = if on { "Codex is connected. Restart any Codex that's open.".to_string() } else { "Codex is disconnected.".to_string() };
    if on {
        let own_notify = lines.iter().any(|l| l.trim_start().starts_with("notify"));
        if own_notify {
            msg = "Codex is connected for MCP. It already has its own notify program, so its \"done\" messages won't reach the bar.".into();
        } else {
            let s = ai().script.display().to_string();
            #[cfg(windows)]
            let cmd = format!("notify = ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', '{s}', 'codex', 'notify'] # cubecursor");
            #[cfg(not(windows))]
            let cmd = format!("notify = ['sh', '{s}', 'codex', 'notify'] # cubecursor");
            // Top-level keys go before any [table].
            lines.insert(0, cmd);
        }
        lines.push(String::new());
        lines.push("[mcp_servers.cubecursor] # cubecursor".into());
        lines.push(format!("url = \"http://127.0.0.1:{PORT}/mcp\" # cubecursor"));
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    write_safely(&p, &(lines.join("\n") + "\n"))?;
    Ok(msg)
}

#[tauri::command]
pub fn ai_status() -> Value {
    let (remote, sync) = opts();
    json!({
        "claude": claude_connected(),
        "codex": codex_connected(),
        "claudeFound": home().join(".claude").exists(),
        "codexFound": home().join(".codex").exists(),
        "key": key_get().is_some(),
        "remote": remote,
        "sync": sync,
        "mcp": format!("http://127.0.0.1:{PORT}/mcp"),
    })
}

#[tauri::command]
pub fn ai_connect(tool: String, on: bool) -> Result<String, String> {
    match tool.as_str() {
        "claude" => claude_set(on),
        "codex" => codex_set(on),
        _ => Err("Unknown tool".into()),
    }
}

// ------------------------------------------------------------------ the local server

fn serve() {
    let l = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("ai: can't listen on {PORT}: {e}");
            return;
        }
    };
    log::info!("ai: listening on 127.0.0.1:{PORT}");
    for s in l.incoming().flatten() {
        std::thread::spawn(move || {
            if let Err(e) = handle(s) {
                log::debug!("ai: request failed: {e}");
            }
        });
    }
}

fn handle(mut s: TcpStream) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let (mut len, mut host, mut origin) = (0usize, String::new(), None::<String>);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            match k.as_str() {
                "content-length" => len = v.parse().unwrap_or(0),
                "host" => host = v,
                "origin" => origin = Some(v),
                _ => {}
            }
        }
    }
    // Only programs on this computer, and never a web page (DNS rebinding).
    let host_ok = host.is_empty() || host.starts_with("127.0.0.1") || host.starts_with("localhost");
    if !host_ok || origin.is_some_and(|o| o != "null") {
        return reply(&mut s, 403, "text/plain", "forbidden");
    }
    let mut body = vec![0u8; len.min(8 << 20)];
    r.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).to_string();
    s.set_read_timeout(None)?;
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => reply(&mut s, 200, "text/plain", "ok"),
        ("GET", p) if p.starts_with("/spotify/callback") => {
            let page = crate::spotify::callback(p.split_once('?').map(|x| x.1).unwrap_or(""));
            reply(&mut s, 200, "text/html; charset=utf-8", &page)
        }
        ("POST", "/mcp") => match mcp(&body) {
            Some(out) => reply(&mut s, 200, "application/json", &out),
            None => reply(&mut s, 202, "text/plain", ""),
        },
        ("GET", "/mcp") | ("DELETE", "/mcp") => reply(&mut s, 405, "text/plain", ""),
        ("POST", p) if p.starts_with("/hook/") => {
            let mut it = p.trim_start_matches("/hook/").splitn(2, '/');
            let (src, ev) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
            let out = on_hook(&src, &ev, &body);
            reply(&mut s, 200, "application/json", &out)
        }
        _ => reply(&mut s, 404, "text/plain", "not found"),
    }
}

fn reply(s: &mut TcpStream, code: u16, ctype: &str, body: &str) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        202 => "Accepted",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        _ => "Not Found",
    };
    write!(s, "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    s.flush()
}

// ------------------------------------------------------------------ agent events

/// The words for what the agent wants to do.
fn describe(tool: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(String::from);
    let t = match tool {
        "Bash" | "PowerShell" => s("command"),
        "Edit" | "Write" | "MultiEdit" | "Read" | "NotebookEdit" => s("file_path"),
        "WebFetch" => s("url"),
        _ => None,
    };
    trunc(&t.unwrap_or_else(|| input.to_string()), 220)
}

/// The agent's last words in a Claude Code transcript.
fn last_words(path: &str) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    use std::io::Seek;
    let _ = f.seek(std::io::SeekFrom::Start(size.saturating_sub(2 << 20)));
    let mut buf = String::new();
    let _ = f.read_to_string(&mut buf);
    for line in buf.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        if let Some(blocks) = v["message"]["content"].as_array() {
            let text: Vec<&str> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            if !text.is_empty() {
                return trunc(&text.join(" "), 280);
            }
        }
    }
    String::new()
}

fn on_hook(src: &str, ev: &str, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let (pc, pc_name) = me();
    let session = v["session_id"].as_str().or(v["thread-id"].as_str()).unwrap_or("").to_string();
    let cwd = v["cwd"].as_str().unwrap_or("");
    let project = Path::new(cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tool_name = v["tool_name"].as_str().unwrap_or("").to_string();
    let (state, text) = match (src, ev) {
        ("claude", "UserPromptSubmit") => ("working", trunc(v["prompt"].as_str().unwrap_or(""), 140)),
        ("claude", "Notification") => {
            let m = v["message"].as_str().unwrap_or("Needs you");
            // The permission itself comes as its own event; this one only repeats it.
            if m.to_lowercase().contains("permission") {
                return "{}".into();
            }
            ("waiting", m.to_string())
        }
        ("claude", "PermissionRequest") => ("approve", describe(&tool_name, &v["tool_input"])),
        ("claude", "Stop") => ("done", last_words(v["transcript_path"].as_str().unwrap_or(""))),
        ("claude", "SessionEnd") => ("ended", String::new()),
        ("codex", _) => match v["type"].as_str() {
            Some("agent-turn-complete") => ("done", trunc(v["last-assistant-message"].as_str().unwrap_or(""), 280)),
            _ => ("waiting", v["type"].as_str().unwrap_or("Needs you").to_string()),
        },
        _ => ("waiting", ev.to_string()),
    };
    let id = new_id();
    let e = json!({
        "id": id, "pc": pc, "pcName": pc_name, "source": src, "session": session,
        "project": project, "state": state, "text": text, "tool": tool_name, "at": now_ms(),
    });
    to_bar("agent", &e);
    relay(None, "agent", &e);
    if state != "approve" {
        return "{}".into();
    }
    let (tx, rx) = mpsc::channel();
    ai().pending.lock().unwrap().insert(id.clone(), tx);
    let answer = rx.recv_timeout(APPROVE_WAIT).ok();
    ai().pending.lock().unwrap().remove(&id);
    // Every bar can drop the card now.
    let mut done = e.clone();
    done["state"] = json!(match answer {
        Some(true) => "allowed",
        Some(false) => "denied",
        None => "terminal",
    });
    to_bar("agent", &done);
    relay(None, "agent", &done);
    match answer {
        Some(true) => json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }).to_string(),
        Some(false) => json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "deny", "message": "Denied from CubeCursor." } } }).to_string(),
        None => "{}".into(),
    }
}

fn resolve(id: &str, allow: bool) {
    if let Some(tx) = ai().pending.lock().unwrap().remove(id) {
        let _ = tx.send(allow);
    }
}

/// The bar's Allow / Deny, on whichever computer it was pressed.
#[tauri::command]
pub fn agent_answer(id: String, pc: String, allow: bool) {
    if pc == me().0 {
        resolve(&id, allow);
    } else {
        relay(Some(pc), "agent_answer", &json!({ "id": id, "allow": allow }));
    }
}

// ------------------------------------------------------------------ relays from the other computers

fn on_relay(m: tandem_core::node::RelayIn) {
    let v: Value = serde_json::from_str(&m.json).unwrap_or(Value::Null);
    match m.kind.as_str() {
        "agent" => to_bar("agent", &v),
        "agent_answer" => resolve(v["id"].as_str().unwrap_or(""), v["allow"].as_bool().unwrap_or(false)),
        "mcp_req" => {
            let id = v["id"].clone();
            let out = if !opts().0 {
                json!({ "id": id, "error": format!("{} doesn't let AI agents use it. Turn on \"Let AI agents use this computer\" in CubeCursor there (Settings → AI).", me().1) })
            } else {
                let tool = v["tool"].as_str().unwrap_or("");
                to_bar("note", &json!(format!("🤖 An AI agent on {} used {} here", m.from_name, tool.replace('_', " "))));
                match run_tool(tool, &v["args"]) {
                    Ok(content) => json!({ "id": id, "content": content }),
                    Err(e) => json!({ "id": id, "error": e }),
                }
            };
            relay(Some(m.from), "mcp_res", &out);
        }
        "mcp_res" => {
            if let Some(tx) = ai().calls.lock().unwrap().remove(v["id"].as_str().unwrap_or("")) {
                let _ = tx.send(v);
            }
        }
        "ai_send" => receive_send(&m.from_name, &v),
        "audio_pull" => {
            if let Some(st) = ai().app.try_state::<AppState>() {
                st.node.hear_audio(m.from.clone(), v["on"].as_bool().unwrap_or(false));
            }
        }
        "instr" => receive_instructions(&v),
        k if k.starts_with("ui:") => to_bar("relay", &json!({ "from": m.from, "fromName": m.from_name, "kind": k, "json": m.json })),
        _ => {}
    }
}

/// The bar's own relays (the prompt library, a video moving over), to one computer or
/// every other one.
#[tauri::command]
pub fn ai_relay(kind: String, json: String, to: Option<String>) {
    if kind.starts_with("ui:") {
        if let Some(st) = ai().app.try_state::<AppState>() {
            st.node.relay(to, kind, json);
        }
    }
}

/// Play this computer's sound on that one's speakers (it asks for it, as "Hear it" does).
#[tauri::command]
pub fn media_play_on(peer_id: String, on: bool) {
    relay(Some(peer_id), "audio_pull", &json!({ "on": on }));
}

// ------------------------------------------------------------------ MCP

const INSTRUCTIONS: &str = "CubeCursor links this person's computers. Use these tools to work on their other computers: run commands or tests there, read and write files, take a screenshot to check how something looks, open a page (a localhost address is rewritten so the other computer reaches this one), or send a file. Call list_computers first to see the names. A computer only accepts requests if the person turned on \"Let AI agents use this computer\" on it.";

fn tools() -> Value {
    let pc = json!({ "type": "string", "description": "The computer's name from list_computers. Leave out for this computer." });
    json!([
        { "name": "list_computers", "description": "The person's computers: this one and the paired ones, and whether each is connected.", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "run_command", "description": "Run a shell command on a computer (cmd on Windows, sh on macOS) and get its output and exit code.",
          "inputSchema": { "type": "object", "properties": { "computer": pc, "command": { "type": "string" }, "cwd": { "type": "string", "description": "Folder to run it in" }, "timeout_seconds": { "type": "number", "description": "Default 60, at most 600" } }, "required": ["command"] } },
        { "name": "read_file", "description": "Read a text file on a computer (up to 2 MB).",
          "inputSchema": { "type": "object", "properties": { "computer": pc, "path": { "type": "string" } }, "required": ["path"] } },
        { "name": "write_file", "description": "Write a text file on a computer (creates folders as needed, replaces the file).",
          "inputSchema": { "type": "object", "properties": { "computer": pc, "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] } },
        { "name": "list_dir", "description": "List a folder on a computer.",
          "inputSchema": { "type": "object", "properties": { "computer": pc, "path": { "type": "string" } }, "required": ["path"] } },
        { "name": "screenshot", "description": "A screenshot of a computer's main screen.",
          "inputSchema": { "type": "object", "properties": { "computer": pc } } },
        { "name": "open_url", "description": "Open a web page in a computer's browser, e.g. to preview a dev server there. localhost/127.0.0.1 is rewritten to this computer's network address (the server must listen on the network, e.g. vite --host).",
          "inputSchema": { "type": "object", "properties": { "computer": pc, "url": { "type": "string" } }, "required": ["url"] } },
        { "name": "send_file", "description": "Send a file or folder from this computer to another one (it lands in its Downloads/CubeCursor).",
          "inputSchema": { "type": "object", "properties": { "computer": pc, "path": { "type": "string" } }, "required": ["computer", "path"] } },
        { "name": "notify", "description": "Show a short message in CubeCursor's top bar, e.g. when a long task is done.",
          "inputSchema": { "type": "object", "properties": { "message": { "type": "string" } }, "required": ["message"] } },
    ])
}

fn mcp(body: &str) -> Option<String> {
    let req: Value = serde_json::from_str(body).ok()?;
    if req.is_array() {
        let out: Vec<Value> = req.as_array()?.iter().filter_map(|r| mcp(&r.to_string()).and_then(|s| serde_json::from_str(&s).ok())).collect();
        return (!out.is_empty()).then(|| Value::Array(out).to_string());
    }
    let id = req.get("id")?.clone();
    let method = req["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "cubecursor", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = req["params"]["arguments"].clone();
            Ok(match call_tool(name, &args) {
                Ok(content) => json!({ "content": content }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            })
        }
        _ => Err(json!({ "code": -32601, "message": format!("Unknown method {method}") })),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    }.to_string())
}

/// Which computer the agent means: None for this one.
fn target(args: &Value) -> Result<Option<(String, String)>, String> {
    let want = args["computer"].as_str().unwrap_or("").trim().to_lowercase();
    let st = ai().app.try_state::<AppState>().ok_or("CubeCursor is starting")?;
    let s = st.node.snapshot();
    if want.is_empty() || want == "this" || want == s.device_name.to_lowercase() || want == s.device_id.to_lowercase() {
        return Ok(None);
    }
    let p = s.paired.iter().find(|p| p.name.to_lowercase() == want || p.id.to_lowercase() == want)
        .ok_or_else(|| format!("No computer called \"{}\". Call list_computers for the names.", args["computer"].as_str().unwrap_or("")))?;
    if !p.connected {
        return Err(format!("{} isn't connected right now.", p.name));
    }
    Ok(Some((p.id.clone(), p.name.clone())))
}

fn call_tool(name: &str, args: &Value) -> Result<Vec<Value>, String> {
    match name {
        "list_computers" => {
            let st = ai().app.try_state::<AppState>().ok_or("CubeCursor is starting")?;
            let s = st.node.snapshot();
            let mut out = format!("{} (this computer)\n", s.device_name);
            for p in &s.paired {
                out += &format!("{} ({})\n", p.name, if p.connected { "connected" } else { "not connected" });
            }
            Ok(vec![json!({ "type": "text", "text": out })])
        }
        "notify" => {
            to_bar("note", &json!(format!("🤖 {}", trunc(args["message"].as_str().unwrap_or(""), 160))));
            Ok(vec![json!({ "type": "text", "text": "Shown." })])
        }
        "send_file" => {
            let Some((id, pname)) = target(args)? else {
                return Err("send_file sends to another computer: name it.".into());
            };
            let path = PathBuf::from(args["path"].as_str().unwrap_or(""));
            if !path.exists() {
                return Err(format!("{} doesn't exist", path.display()));
            }
            let st = ai().app.try_state::<AppState>().ok_or("CubeCursor is starting")?;
            st.node.send_files_to(vec![path], id);
            Ok(vec![json!({ "type": "text", "text": format!("Sending to {pname}; it lands in its Downloads/CubeCursor folder.") })])
        }
        _ => match target(args)? {
            None => run_tool(name, args),
            Some((id, _)) => {
                let mut args = args.clone();
                if name == "open_url" {
                    if let (Some(u), Some(ip)) = (args["url"].as_str(), lan_ip()) {
                        args["url"] = json!(u.replace("://localhost", &format!("://{ip}")).replace("://127.0.0.1", &format!("://{ip}")));
                    }
                }
                let cid = new_id();
                let (tx, rx) = mpsc::channel();
                ai().calls.lock().unwrap().insert(cid.clone(), tx);
                relay(Some(id), "mcp_req", &json!({ "id": cid, "tool": name, "args": args }));
                let wait = Duration::from_secs(args["timeout_seconds"].as_u64().unwrap_or(60).clamp(5, 600) + 15);
                let res = rx.recv_timeout(wait);
                ai().calls.lock().unwrap().remove(&cid);
                let v = res.map_err(|_| "The other computer didn't answer in time.".to_string())?;
                if let Some(e) = v["error"].as_str() {
                    return Err(e.to_string());
                }
                Ok(v["content"].as_array().cloned().unwrap_or_default())
            }
        },
    }
}

fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// A tool, run on this computer.
fn run_tool(name: &str, args: &Value) -> Result<Vec<Value>, String> {
    let text = |t: String| Ok(vec![json!({ "type": "text", "text": t })]);
    let path = || PathBuf::from(args["path"].as_str().unwrap_or(""));
    match name {
        "run_command" => text(run_command(
            args["command"].as_str().ok_or("command is missing")?,
            args["cwd"].as_str(),
            args["timeout_seconds"].as_u64().unwrap_or(60).clamp(1, 600),
        )?),
        "read_file" => {
            let p = path();
            let m = std::fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            if m.len() > 2 << 20 {
                return Err(format!("{} is {} MB; read_file reads up to 2 MB", p.display(), m.len() >> 20));
            }
            text(String::from_utf8_lossy(&std::fs::read(&p).map_err(|e| e.to_string())?).to_string())
        }
        "write_file" => {
            let p = path();
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            let c = args["content"].as_str().unwrap_or("");
            std::fs::write(&p, c).map_err(|e| format!("{}: {e}", p.display()))?;
            text(format!("Wrote {} bytes to {}", c.len(), p.display()))
        }
        "list_dir" => {
            let p = path();
            let mut rows: Vec<String> = std::fs::read_dir(&p).map_err(|e| format!("{}: {e}", p.display()))?
                .flatten()
                .map(|e| {
                    let m = e.metadata().ok();
                    let dir = m.as_ref().is_some_and(|m| m.is_dir());
                    format!("{}{}\t{}", e.file_name().to_string_lossy(), if dir { "/" } else { "" }, if dir { String::new() } else { m.map(|m| m.len().to_string()).unwrap_or_default() })
                })
                .collect();
            rows.sort();
            rows.truncate(1000);
            text(rows.join("\n"))
        }
        "screenshot" => {
            #[cfg(windows)]
            {
                let png = tandem_core::win::screenshot_png().ok_or("Couldn't take a screenshot")?;
                Ok(vec![json!({ "type": "image", "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png) })])
            }
            #[cfg(not(windows))]
            Err("Screenshots work on Windows for now".into())
        }
        "open_url" => {
            let u = args["url"].as_str().unwrap_or("");
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err("Only http and https addresses".into());
            }
            open_url(u)?;
            to_bar("note", &json!(format!("🌐 Opened {}", trunc(u, 60))));
            text(format!("Opened {u}"))
        }
        _ => Err(format!("Unknown tool {name}")),
    }
}

fn open_url(u: &str) -> Result<(), String> {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", u]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(u).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(u).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn run_command(cmd: &str, cwd: Option<&str>, secs: u64) -> Result<String, String> {
    #[cfg(windows)]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").raw_arg(cmd).creation_flags(0x0800_0000);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    if let Some(d) = cwd.filter(|d| !d.is_empty()) {
        c.current_dir(d);
    }
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let ro = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let re = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(s);
        }
        if std::time::Instant::now() > end {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (o, e) = (ro.join().unwrap_or_default(), re.join().unwrap_or_default());
    let mut out = String::from_utf8_lossy(&o).to_string();
    if !e.is_empty() {
        out += &format!("\n[stderr]\n{}", String::from_utf8_lossy(&e));
    }
    if out.len() > 60_000 {
        let cut = out.char_indices().nth(60_000).map(|(i, _)| i).unwrap_or(out.len());
        out.truncate(cut);
        out += "\n…(cut)";
    }
    Ok(match status {
        Some(s) => format!("{out}\n[exit code {}]", s.code().unwrap_or(-1)),
        None => format!("{out}\n[stopped after {secs} s]"),
    })
}

// ------------------------------------------------------------------ usage meter

/// Seconds since 1970 from "YYYY-MM-DDTHH:MM:SS…Z".
fn parse_ts(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |a: usize, z: usize| t.get(a..z)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss) as u64)
}

/// $ per million tokens: input, output, cache read, cache write. Anthropic list prices.
fn price(model: &str) -> Option<[f64; 4]> {
    let m = model.to_lowercase();
    Some(if m.contains("fable") || m.contains("mythos") {
        [10.0, 50.0, 0.25, 12.5]
    } else if m.contains("opus-5-5") {
        [4.0, 20.0, 0.2, 5.0]
    } else if m.contains("opus") {
        [5.0, 25.0, 0.5, 6.25]
    } else if m.contains("sonnet-5") {
        [2.0, 10.0, 0.2, 2.5]
    } else if m.contains("sonnet") {
        [3.0, 15.0, 0.3, 3.75]
    } else if m.contains("haiku-5") {
        [0.1, 0.5, 0.01, 0.125]
    } else if m.contains("haiku") {
        [1.0, 5.0, 0.1, 1.25]
    } else {
        return None;
    })
}

fn recent_files(dir: &Path, since: SystemTime, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            if depth > 0 {
                recent_files(&p, since, out, depth - 1);
            }
        } else if p.extension().is_some_and(|x| x == "jsonl") && m.modified().is_ok_and(|t| t >= since) {
            out.push(p);
        }
    }
}

/// Tokens (and the cost at API prices) over the last 24 hours, from Claude Code's and
/// Codex's own logs on this computer.
#[tauri::command]
pub async fn ai_usage() -> Value {
    tauri::async_runtime::spawn_blocking(usage).await.unwrap_or(Value::Null)
}

fn usage() -> Value {
    let now = SystemTime::now();
    let since = now - Duration::from_secs(86_400);
    let since_s = since.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut files = vec![];
    recent_files(&home().join(".claude").join("projects"), since, &mut files, 3);
    let (mut c_in, mut c_out, mut c_cr, mut c_cw, mut cost) = (0u64, 0u64, 0u64, 0u64, 0f64);
    let mut seen = HashSet::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for line in text.lines() {
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if parse_ts(v["timestamp"].as_str().unwrap_or("")).is_none_or(|t| t < since_s) {
                continue;
            }
            let msg = &v["message"];
            // A message is logged once per content block: count it once.
            let key = format!("{}:{}", msg["id"].as_str().unwrap_or(""), v["requestId"].as_str().unwrap_or(""));
            if !seen.insert(key) {
                continue;
            }
            let u = &msg["usage"];
            let g = |k: &str| u[k].as_u64().unwrap_or(0);
            let (i, o, cr, cw) = (g("input_tokens"), g("output_tokens"), g("cache_read_input_tokens"), g("cache_creation_input_tokens"));
            c_in += i;
            c_out += o;
            c_cr += cr;
            c_cw += cw;
            if let Some(p) = price(msg["model"].as_str().unwrap_or("")) {
                cost += (i as f64 * p[0] + o as f64 * p[1] + cr as f64 * p[2] + cw as f64 * p[3]) / 1e6;
            }
        }
    }
    let mut cx = vec![];
    recent_files(&home().join(".codex").join("sessions"), since, &mut cx, 4);
    let (mut x_in, mut x_out, mut x_cached) = (0u64, 0u64, 0u64);
    for f in &cx {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        // Each session's running total: its last one counts.
        if let Some(t) = text.lines().rev().filter(|l| l.contains("total_token_usage")).find_map(|l| serde_json::from_str::<Value>(l).ok()) {
            let u = &t["payload"]["info"]["total_token_usage"];
            x_in += u["input_tokens"].as_u64().unwrap_or(0);
            x_out += u["output_tokens"].as_u64().unwrap_or(0);
            x_cached += u["cached_input_tokens"].as_u64().unwrap_or(0);
        }
    }
    json!({
        "claude": { "input": c_in, "output": c_out, "cacheRead": c_cr, "cacheWrite": c_cw, "cost": cost, "sessions": files.len() },
        "codex": { "input": x_in, "output": x_out, "cached": x_cached, "sessions": cx.len() },
    })
}

// ------------------------------------------------------------------ Ask Bob

#[cfg(windows)]
fn key_entry() -> Option<keyring::Entry> {
    keyring::Entry::new("CubeCursor", "anthropic-api-key").ok()
}
fn key_get() -> Option<String> {
    #[cfg(windows)]
    return key_entry()?.get_password().ok().filter(|k| !k.is_empty());
    #[cfg(not(windows))]
    return std::fs::read_to_string(home().join(".cubecursor-anthropic-key")).ok().map(|s| s.trim().to_string()).filter(|k| !k.is_empty());
}

#[tauri::command]
pub fn ai_key_set(key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    #[cfg(windows)]
    {
        let e = key_entry().ok_or("No credential store")?;
        if key.is_empty() {
            let _ = e.delete_credential();
            Ok(())
        } else {
            e.set_password(&key).map_err(|e| e.to_string())
        }
    }
    #[cfg(not(windows))]
    {
        let p = home().join(".cubecursor-anthropic-key");
        if key.is_empty() {
            let _ = std::fs::remove_file(p);
            Ok(())
        } else {
            std::fs::write(p, key).map_err(|e| e.to_string())
        }
    }
}

const BOB: &str = "You are Bob, the small friendly cube who lives in CubeCursor's top bar. The person is usually a developer. Answer in plain text (no Markdown headings or tables), short and to the point: a few sentences, or a short code snippet when that is the answer.";

#[tauri::command]
pub async fn ai_ask(question: String) -> Result<String, String> {
    let key = key_get().ok_or("Add your Anthropic API key in Settings → AI first.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(120)).build();
        let body = json!({
            "model": "claude-opus-5-5",
            "max_tokens": 16000,
            "output_config": { "effort": "low" },
            "fallbacks": "default",
            "system": BOB,
            "messages": [{ "role": "user", "content": question }],
        });
        let res = agent
            .post("https://api.anthropic.com/v1/messages")
            .set("x-api-key", &key)
            .set("anthropic-version", "2023-06-01")
            .set("anthropic-beta", "server-side-fallback-2026-07-01")
            .send_json(body);
        let v: Value = match res {
            Ok(r) => r.into_json().map_err(|e| e.to_string())?,
            Err(ureq::Error::Status(code, r)) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                let m = v["error"]["message"].as_str().unwrap_or("").to_string();
                return Err(match code {
                    401 => "That API key didn't work. Check it in Settings → AI.".into(),
                    429 => "Too many questions at once. Try again in a moment.".into(),
                    _ => format!("Claude said: {code} {m}"),
                });
            }
            Err(e) => return Err(format!("Couldn't reach Claude: {e}")),
        };
        if v["stop_reason"] == "refusal" {
            return Err("Claude can't help with that one.".into());
        }
        let text: Vec<&str> = v["content"].as_array().map(|a| a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()).unwrap_or_default();
        Ok(text.join("\n").trim().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------------ clipboard: errors, sending to an AI

/// Does this copied text look like an error or stack trace?
fn looks_like_error(t: &str) -> bool {
    if t.len() < 30 || t.len() > 30_000 {
        return false;
    }
    let lower = t.to_lowercase();
    let traces = t.contains("Traceback (most recent call last)")
        || t.contains("panicked at")
        || t.lines().any(|l| l.trim_start().starts_with("error[E") || l.trim_start().starts_with("error TS"))
        || t.contains("npm ERR!")
        || t.contains("Exception in thread")
        || t.contains("Unhandled exception")
        || t.contains("Segmentation fault")
        || (t.contains("Error") && (t.contains("\n    at ") || t.contains("\n\tat ") || t.contains(".py\", line")))
        || (lower.contains("error:") && lower.lines().count() > 1 && (lower.contains(".rs:") || lower.contains(".ts:") || lower.contains(".js:") || lower.contains(".cs(") || lower.contains(".java:")));
    traces
}

fn watch_clipboard() {
    #[cfg(windows)]
    {
        let mut last_seq = tandem_core::win::clip_seq();
        let mut last_text = String::new();
        loop {
            std::thread::sleep(Duration::from_millis(1200));
            let seq = tandem_core::win::clip_seq();
            if seq == last_seq {
                continue;
            }
            last_seq = seq;
            if let Some(t) = tandem_core::win::clip_text() {
                if t != last_text && looks_like_error(&t) {
                    last_text = t.clone();
                    to_bar("clip-error", &json!({ "text": trunc(&t, 8000) }));
                }
            }
        }
    }
}

/// Puts text on this computer's clipboard (a prompt, a fix).
#[tauri::command]
pub fn ai_copy(text: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        tandem_core::win::clip_set(&tandem_core::platform::ClipContent::Text(text));
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Err("Copying works on Windows for now".into())
    }
}

/// Ctrl+Alt+A (what you copied) or Ctrl+Alt+S (a screenshot): onto the other computer's
/// clipboard, ready to paste into Claude, ChatGPT or Codex there.
pub fn hotkey_send(app: &AppHandle, shot: bool) {
    let Some(st) = app.try_state::<AppState>() else { return };
    let s = st.node.snapshot();
    let Some(peer) = s.paired.iter().find(|p| p.connected) else {
        to_bar("note", &json!("No other computer is connected."));
        return;
    };
    #[cfg(windows)]
    {
        use tandem_core::platform::ClipContent;
        let msg = if shot {
            match tandem_core::win::screenshot_png() {
                Some(png) => json!({ "png": base64::engine::general_purpose::STANDARD.encode(png), "shot": true }),
                None => return to_bar("note", &json!("Couldn't take a screenshot.")),
            }
        } else {
            match tandem_core::win::clip_get() {
                Some(ClipContent::Text(t)) => json!({ "text": t }),
                Some(ClipContent::Image(png)) => json!({ "png": base64::engine::general_purpose::STANDARD.encode(png) }),
                Some(ClipContent::Files(f)) => {
                    st.node.send_files_to(f, peer.id.clone());
                    return to_bar("note", &json!(format!("📎 Sending your files to {}", peer.name)));
                }
                None => return to_bar("note", &json!("Copy something first (Ctrl+C), then press Ctrl+Alt+A.")),
            }
        };
        relay(Some(peer.id.clone()), "ai_send", &msg);
        to_bar("ai-sent", &json!({ "to": peer.name, "shot": shot }));
    }
    #[cfg(not(windows))]
    {
        let _ = shot;
        to_bar("note", &json!(format!("Sending to {} works from Windows for now.", peer.name)));
    }
}

fn receive_send(from: &str, v: &Value) {
    #[cfg(windows)]
    {
        use tandem_core::platform::ClipContent;
        let (content, preview, image) = if let Some(t) = v["text"].as_str() {
            (ClipContent::Text(t.to_string()), trunc(t, 160), false)
        } else if let Some(b) = v["png"].as_str().and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()) {
            (ClipContent::Image(b), if v["shot"].as_bool() == Some(true) { "A screenshot".into() } else { "A picture".into() }, true)
        } else {
            return;
        };
        tandem_core::win::clip_set(&content);
        to_bar("ai-in", &json!({ "from": from, "preview": preview, "image": image }));
    }
    #[cfg(not(windows))]
    let _ = (from, v);
}

// ------------------------------------------------------------------ CLAUDE.md / AGENTS.md on every computer

fn instr_files() -> [(&'static str, PathBuf); 2] {
    [("claude", home().join(".claude").join("CLAUDE.md")), ("codex", home().join(".codex").join("AGENTS.md"))]
}
fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}
static SENT: Mutex<Option<HashMap<&'static str, u64>>> = Mutex::new(None);

fn sync_instructions() {
    let mut peers_before = 0usize;
    loop {
        std::thread::sleep(Duration::from_secs(20));
        if !opts().1 {
            continue;
        }
        // A computer that just connected gets the files again.
        let peers = ai().app.try_state::<AppState>().map(|s| s.node.snapshot().paired.iter().filter(|p| p.connected).count()).unwrap_or(0);
        let fresh = peers > peers_before;
        peers_before = peers;
        for (name, p) in instr_files() {
            let t = mtime(&p);
            if t == 0 {
                continue;
            }
            let mut sent = SENT.lock().unwrap();
            let last = sent.get_or_insert_with(HashMap::new).entry(name).or_insert(0);
            if t > *last || fresh {
                *last = t;
                if let Ok(text) = std::fs::read_to_string(&p) {
                    relay(None, "instr", &json!({ "name": name, "text": text, "mtime": t }));
                }
            }
        }
    }
}

fn receive_instructions(v: &Value) {
    if !opts().1 {
        return;
    }
    let (Some(name), Some(text), Some(t)) = (v["name"].as_str(), v["text"].as_str(), v["mtime"].as_u64()) else { return };
    let Some((key, p)) = instr_files().into_iter().find(|(n, _)| *n == name) else { return };
    if mtime(&p) >= t || std::fs::read_to_string(&p).is_ok_and(|old| old == text) {
        return;
    }
    if write_safely(&p, text).is_ok() {
        // Not sent straight back.
        SENT.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, mtime(&p));
        to_bar("note", &json!(format!("📝 {} updated from your other computer", p.file_name().unwrap_or_default().to_string_lossy())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_lists_its_tools_and_ignores_notifications() {
        let init: Value = serde_json::from_str(&mcp(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#).unwrap()).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "cubecursor");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        let list: Value = serde_json::from_str(&mcp(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap()).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"run_command") && names.contains(&"screenshot"));
        assert!(mcp(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let bad: Value = serde_json::from_str(&mcp(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#).unwrap()).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    #[test]
    fn errors_are_recognised() {
        assert!(looks_like_error("Traceback (most recent call last):\n  File \"a.py\", line 3, in <module>\nKeyError: 'x'"));
        assert!(looks_like_error("TypeError: Cannot read properties of undefined (reading 'x')\n    at foo (app.js:10:5)"));
        assert!(looks_like_error("error[E0063]: missing fields `a` in initializer of `UiOptions`\n --> src/lib.rs:73:9"));
        assert!(!looks_like_error("Meeting notes: we had no errors today, everything went fine."));
        assert!(!looks_like_error("short"));
    }

    #[test]
    fn timestamps_parse() {
        assert_eq!(parse_ts("1970-01-02T00:00:00.000Z"), Some(86_400));
        assert_eq!(parse_ts("2026-11-02T10:30:00Z"), Some(1_793_615_400));
        assert_eq!(parse_ts("bad"), None);
    }

    #[test]
    fn prices_by_model() {
        assert_eq!(price("claude-opus-5-5").unwrap()[0], 4.0);
        assert_eq!(price("claude-sonnet-5-5").unwrap()[1], 10.0);
        assert!(price("gpt-5").is_none());
    }
}
