//! `tandem` — terminal front-end for tandem-core.
//!
//!   tandem [--name NAME] [--state FILE] [--port N]
//!
//! then type: help

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tandem_core::node::*;
use tandem_core::proto::{Edge, DEFAULT_TCP_PORT, DISCOVERY_PORT};
use tokio::io::{AsyncBufReadExt, BufReader};

/// JSON file store. NOTE: the link keys are stored in clear text here — fine for
/// a debugging tool; the desktop app keeps them in the Windows Credential Manager.
struct FileStore(PathBuf);
impl Store for FileStore {
    fn load(&self) -> Option<Persisted> {
        serde_json::from_slice(&std::fs::read(&self.0).ok()?).ok()
    }
    fn save(&self, p: &Persisted) {
        if let Some(d) = self.0.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&self.0, serde_json::to_vec_pretty(p).unwrap_or_default());
    }
}

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn edge(s: &str) -> Option<Edge> {
    Edge::parse(s)
}

const HELP: &str = "\
commands:
  status                     show state
  nearby                     computers seen on the LAN
  pair <ip> <code> [port]    pair with a computer showing its code
  connect <id>               reconnect to a paired computer
  side <left|right|top|bottom>   where the other computer sits relative to this one
  send <path>...             send files/folders to the other computer
  engage                     jump to the other computer now (Scroll Lock does this too)
  disconnect | forget <id> | newcode | quit
Scroll Lock = jump / emergency exit. Copy/paste text and files syncs automatically.";

fn print_status(s: &Snapshot) {
    println!(
        "— {} ({}) — control: {}",
        s.device_name, s.device_id, s.control
    );
    println!("  pairing code: {}   (port {})", s.pairing_code, s.tcp_port);
    if let Some(c) = &s.connected {
        println!("  connected to: {} ({})", c.name, c.id);
    }
    for p in &s.paired {
        println!(
            "  paired: {} ({}) side={:?}{}{}",
            p.name,
            p.id,
            p.side,
            if p.connected { " [connected]" } else { "" },
            if p.nearby { " [nearby]" } else { "" }
        );
    }
    for t in &s.transfers {
        println!(
            "  transfer {} {} {}/{} {} {}",
            t.direction, t.name, t.bytes, t.total, t.state, t.message
        );
    }
    if let Some(e) = &s.error {
        println!("  ! {e}");
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("Tandem");
    let state = arg(&args, "--state")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("cli-state.json"));
    let name = arg(&args, "--name")
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "This computer".into());
    let downloads = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join("Downloads").join("Tandem"))
        .unwrap_or_else(|| dir.join("received"));

    let node = Node::start(
        NodeConfig {
            tcp_port: arg(&args, "--port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_TCP_PORT),
            udp_port: DISCOVERY_PORT,
            temp_dir: std::env::temp_dir().join("Tandem"),
            download_dir: downloads,
            discovery: true,
            default_name: name,
            license_key: None,
        },
        tandem_core::default_platform(),
        Arc::new(FileStore(state)),
    )
    .await
    .expect("start");

    #[cfg(not(windows))]
    println!("(not Windows: mouse/keyboard capture is simulated; pairing, clipboard text and files still work)");
    println!("Tandem running. Type 'help'.");
    print_status(&node.snapshot());

    // Print a line whenever something interesting changes.
    let mut rx = node.subscribe();
    tokio::spawn(async move {
        let mut last = String::new();
        while rx.changed().await.is_ok() {
            let s = rx.borrow().clone();
            let line = format!(
                "{} | {} | {:?} | {}",
                s.control,
                s.connected.as_ref().map(|c| c.name.as_str()).unwrap_or("-"),
                s.error,
                s.transfers
                    .iter()
                    .map(|t| format!("{}:{}", t.name, t.state))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if line != last {
                println!("[{line}]");
                last = line;
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            l = lines.next_line() => {
                let Ok(Some(l)) = l else { break };
                let p: Vec<&str> = l.split_whitespace().collect();
                match p.as_slice() {
                    [] => {}
                    ["help"] => println!("{HELP}"),
                    ["status"] => print_status(&node.snapshot()),
                    ["nearby"] => {
                        for n in node.snapshot().nearby {
                            println!("  {} ({}) at {}:{}{}", n.name, n.id, n.addr, n.port, if n.paired { " [paired]" } else { "" });
                        }
                    }
                    ["pair", ip, code, rest @ ..] => match ip.parse::<IpAddr>() {
                        Ok(ip) => node.connect(ip, rest.first().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_TCP_PORT), code.to_string()),
                        Err(_) => println!("bad ip"),
                    },
                    ["connect", id] => node.connect_known(id.to_string()),
                    ["side", s] => match (edge(s), node.snapshot().connected) {
                        (Some(e), Some(c)) => node.set_side(c.id, e),
                        _ => println!("usage: side <left|right|top|bottom> (while connected)"),
                    },
                    ["send", paths @ ..] if !paths.is_empty() => node.send_files(paths.iter().map(PathBuf::from).collect()),
                    ["engage"] => node.engage(),
                    ["disconnect"] => node.disconnect(),
                    ["forget", id] => node.forget(id.to_string()),
                    ["newcode"] => node.new_pairing_code(),
                    ["quit"] | ["exit"] => break,
                    _ => println!("unknown command — try 'help'"),
                }
            }
        }
    }
    node.disconnect();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}
