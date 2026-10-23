//! The engine. One `Node` per app: it listens, discovers, pairs, and runs a
//! single actor that owns "who controls the cursor right now", clipboard sync
//! and file offers.

use crate::channel::*;
use crate::discovery::{self, Discovered};
use crate::error::{Error, Result};
use crate::files::{self, Offer};
use crate::platform::*;
use crate::proto::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{watch, Semaphore};
use tokio::task::AbortHandle;

const MAX_CLIP_TEXT: usize = 8 * 1024 * 1024;
const MAX_CLIP_IMAGE: usize = 12 * 1024 * 1024;
const SESSION_TIMEOUT: Duration = Duration::from_secs(5);
/// The same while one computer's mouse is on the other (pings go every second).
const SESSION_TIMEOUT_BUSY: Duration = Duration::from_millis(2600);
const NEARBY_TTL: Duration = Duration::from_secs(8);

// ---------------------------------------------------------------- config

#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub tcp_port: u16,
    pub udp_port: u16,
    /// Where clipboard-copied files land on the receiving side.
    pub temp_dir: PathBuf,
    /// Where explicitly sent files land.
    pub download_dir: PathBuf,
    pub discovery: bool,
    /// Name shown to the other computer (first run only; then persisted).
    pub default_name: String,
    /// The key licence passes are checked with. `None`: no licence is needed (tests,
    /// the command-line tool). With a key, two computers only stay connected when at
    /// least one of them shows a valid pass (see `license`).
    pub license_key: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairedPeer {
    pub id: String,
    pub name: String,
    /// 32-byte link key, hex. The app may keep this in the OS credential store.
    pub link_key: String,
    /// Which edge of *my* screen leads to this peer.
    pub side: Edge,
    pub last_addr: Option<String>,
    /// Its network card's address, to wake it up while it sleeps.
    #[serde(default)]
    pub mac: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Persisted {
    pub device_id: String,
    pub device_name: String,
    pub peers: Vec<PairedPeer>,
    pub clipboard_sync: bool,
    /// Clipboard files above this size are not fetched automatically.
    pub auto_file_limit_mb: u64,
    /// No edge switching while a full-screen app (a game) is in front.
    #[serde(default = "yes")]
    pub game_mode: bool,
    /// Milliseconds the cursor must keep pushing at the edge before it crosses.
    #[serde(default)]
    pub switch_delay_ms: u32,
    /// Dragging a file to a screen edge carries it over to the next computer.
    #[serde(default = "yes")]
    pub drag_files: bool,
    /// Shift + dragging a program's window across the edge opens it on the next computer.
    /// Off until the person turns it on.
    #[serde(default)]
    pub move_apps: bool,
    /// Second-monitor mode: a window dragged across stays here and is shown live on the
    /// other computer, instead of its program moving there.
    #[serde(default)]
    pub monitor_mode: bool,
}

impl Persisted {
    /// Dragging windows across the edge does something (either mode).
    pub fn spans(&self) -> bool {
        self.move_apps || self.monitor_mode
    }
}

fn yes() -> bool {
    true
}

pub trait Store: Send + Sync {
    fn load(&self) -> Option<Persisted>;
    fn save(&self, p: &Persisted);
}

#[derive(Default)]
pub struct MemoryStore(pub Mutex<Option<Persisted>>);
impl Store for MemoryStore {
    fn load(&self) -> Option<Persisted> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, p: &Persisted) {
        *self.0.lock().unwrap() = Some(p.clone());
    }
}

// ---------------------------------------------------------------- snapshot (UI view)

#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct PeerBrief {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PairedView {
    pub id: String,
    pub name: String,
    pub side: Edge,
    pub connected: bool,
    pub nearby: bool,
    /// How it is doing, while connected.
    pub device: Option<DeviceInfo>,
    /// Round trip to it, in milliseconds (connection quality).
    pub ping_ms: Option<u32>,
    /// Its colour ("#rrggbb"): its card, its glow at the edge, its Bob.
    pub color: String,
    /// It can be woken over the network (its address is known).
    pub wakeable: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct NearbyView {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub port: u16,
    pub paired: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TransferView {
    pub id: u64,
    pub name: String,
    /// "in" | "out"
    pub direction: String,
    pub bytes: u64,
    pub total: u64,
    /// "active" | "done" | "error"
    pub state: String,
    pub message: String,
    pub path: Option<String>,
    /// The other computer's name (sender of an incoming transfer, target of an outgoing one).
    pub peer: String,
}

/// Files being carried across the edge, for the little picture under the cursor.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CarryView {
    pub name: String,
    pub count: usize,
    pub folder: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct Snapshot {
    /// Set on the computer the files are being carried *to*, until they are let go.
    pub carry: Option<CarryView>,
    pub device_id: String,
    pub device_name: String,
    /// Shown as `123-456`; the other computer types it to pair.
    pub pairing_code: String,
    pub tcp_port: u16,
    /// "offline" | "local" | "remote" | "controlled"
    pub control: String,
    /// The computer the cursor is on / that is driving us, if any.
    pub control_peer: Option<String>,
    /// One connected computer: the one in control, otherwise the first by name. Kept for
    /// simple UIs; `paired[].connected` lists them all.
    pub connected: Option<PeerBrief>,
    pub paired: Vec<PairedView>,
    pub nearby: Vec<NearbyView>,
    pub transfers: Vec<TransferView>,
    pub clipboard_sync: bool,
    pub game_mode: bool,
    pub switch_delay_ms: u32,
    pub drag_files: bool,
    pub move_apps: bool,
    pub monitor_mode: bool,
    /// This computer shows a valid licence pass (it can be a "main").
    pub licensed: bool,
    pub error: Option<String>,
    /// A short, self-clearing message for the user ("control came back because…").
    pub notice: Option<String>,
    /// The last move that can still be undone ("Discord to Studio-PC"), for two minutes.
    pub undo: Option<String>,
    /// Second-monitor mode: windows shown across (ours over there and theirs here), for
    /// the "bring windows back" button.
    #[serde(default)]
    pub windows_away: u32,
    /// How this computer is doing.
    pub me: DeviceInfo,
    /// This computer's colour ("#rrggbb").
    pub my_color: String,
}

/// Every computer gets a colour of its own, the same on all of them (from its id).
const PALETTE: [[u8; 3]; 8] = [
    [0x2D, 0xD4, 0xBF], // teal
    [0xFB, 0xBF, 0x24], // amber
    [0xA7, 0x8B, 0xFA], // violet
    [0xFB, 0x71, 0x85], // rose
    [0x38, 0xBD, 0xF8], // sky
    [0xA3, 0xE6, 0x35], // lime
    [0xFB, 0x92, 0x3C], // orange
    [0x81, 0x8C, 0xF8], // indigo
];

pub fn device_rgb(id: &str) -> [u8; 3] {
    // FNV-1a: stable everywhere, no dependencies.
    let mut h: u32 = 0x811c_9dc5;
    for b in id.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

fn css(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// What we know about how each computer is doing.
#[derive(Default)]
struct Devices {
    me: DeviceInfo,
    peers: HashMap<String, (DeviceInfo, Option<u32>)>,
}

// ---------------------------------------------------------------- shared state

struct PairingState {
    code: String,
    attempts_left: u8,
}

#[derive(Default)]
struct Status {
    control: &'static str,
    control_peer: Option<String>,
    /// Every connected computer, in a stable order (by name).
    connected: Vec<PeerBrief>,
    error: Option<String>,
    notice: Option<(String, Instant)>,
    /// What the last move was (for the undo button), and when.
    undo: Option<(String, Instant)>,
    /// Windows shown across (see `Snapshot::windows_away`).
    windows: u32,
    carry: Option<CarryView>,
    /// This computer has a valid licence pass (or no licence is needed).
    licensed: bool,
}

struct Shared {
    me: Identity,
    cfg: NodeConfig,
    platform: Arc<dyn Platform>,
    store: Arc<dyn Store>,
    persisted: Mutex<Persisted>,
    pairing: Mutex<PairingState>,
    discovered: Mutex<HashMap<String, (Discovered, Instant)>>,
    offers: Mutex<VecDeque<(u64, Arc<Offer>)>>,
    transfers: Mutex<Vec<TransferView>>,
    status: Mutex<Status>,
    devices: Mutex<Devices>,
    next_sid: AtomicU64,
    snap_tx: watch::Sender<Snapshot>,
}

fn new_code() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

impl ServerPolicy for Shared {
    fn link_key(&self, peer_id: &str) -> Option<[u8; 32]> {
        self.persisted
            .lock()
            .unwrap()
            .peers
            .iter()
            .find(|p| p.id == peer_id)
            .and_then(|p| unhex32(&p.link_key))
    }
    fn pairing_code(&self) -> Option<String> {
        Some(self.pairing.lock().unwrap().code.clone())
    }
    fn pair_failed(&self) {
        let mut p = self.pairing.lock().unwrap();
        p.attempts_left = p.attempts_left.saturating_sub(1);
        if p.attempts_left == 0 {
            p.code = new_code();
            p.attempts_left = 3;
        }
        drop(p);
        self.publish();
    }
    fn pair_succeeded(&self) {
        let mut p = self.pairing.lock().unwrap();
        p.code = new_code();
        p.attempts_left = 3;
        drop(p);
        self.publish();
    }
}

impl Shared {
    fn snapshot(&self) -> Snapshot {
        let per = self.persisted.lock().unwrap().clone();
        let st = self.status.lock().unwrap();
        let disc = self.discovered.lock().unwrap();
        let fresh = |d: &(Discovered, Instant)| d.1.elapsed() < NEARBY_TTL;
        let code = self.pairing.lock().unwrap().code.clone();
        let devs = self.devices.lock().unwrap();
        let primary = st
            .control_peer
            .as_ref()
            .and_then(|id| st.connected.iter().find(|c| &c.id == id))
            .or_else(|| st.connected.first())
            .cloned();
        let mut nearby: Vec<NearbyView> = disc
            .values()
            .filter(|d| fresh(d))
            .map(|(d, _)| NearbyView {
                id: d.id.clone(),
                name: d.name.clone(),
                addr: d.addr.to_string(),
                port: d.port,
                paired: per.peers.iter().any(|p| p.id == d.id),
            })
            .collect();
        nearby.sort_by(|a, b| a.name.cmp(&b.name));
        Snapshot {
            device_id: per.device_id.clone(),
            device_name: per.device_name.clone(),
            pairing_code: format!("{}-{}", &code[..3], &code[3..]),
            tcp_port: self.me.listen_port,
            control: if st.connected.is_empty() {
                "offline".into()
            } else {
                st.control.to_string()
            },
            control_peer: if st.connected.is_empty() {
                None
            } else {
                st.control_peer.clone()
            },
            connected: primary,
            paired: per
                .peers
                .iter()
                .map(|p| {
                    let connected = st.connected.iter().any(|c| c.id == p.id);
                    let (device, ping_ms) = match devs.peers.get(&p.id) {
                        Some((d, ms)) if connected => (Some(d.clone()), *ms),
                        _ => (None, None),
                    };
                    PairedView {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        side: p.side,
                        connected,
                        nearby: disc.get(&p.id).map(fresh).unwrap_or(false),
                        device,
                        ping_ms,
                        color: css(device_rgb(&p.id)),
                        wakeable: p.mac.is_some(),
                    }
                })
                .collect(),
            nearby,
            transfers: self.transfers.lock().unwrap().clone(),
            clipboard_sync: per.clipboard_sync,
            game_mode: per.game_mode,
            switch_delay_ms: per.switch_delay_ms,
            drag_files: per.drag_files,
            move_apps: per.move_apps,
            monitor_mode: per.monitor_mode,
            licensed: st.licensed,
            error: st.error.clone(),
            notice: st.notice.as_ref().map(|(m, _)| m.clone()),
            undo: st
                .undo
                .as_ref()
                .filter(|(_, at)| at.elapsed() < UNDO_FOR)
                .map(|(m, _)| m.clone()),
            windows_away: st.windows,
            carry: st.carry.clone(),
            me: devs.me.clone(),
            my_color: css(device_rgb(&per.device_id)),
        }
    }

    fn publish(&self) {
        let s = self.snapshot();
        self.snap_tx.send_if_modified(|cur| {
            if *cur != s {
                *cur = s;
                true
            } else {
                false
            }
        });
    }

    fn save(&self) {
        let p = self.persisted.lock().unwrap().clone();
        self.store.save(&p);
    }

    fn set_error(&self, e: Option<String>) {
        self.status.lock().unwrap().error = e;
        self.publish();
    }

    fn upsert_transfer(&self, t: TransferView) {
        let mut v = self.transfers.lock().unwrap();
        if let Some(x) = v
            .iter_mut()
            .find(|x| x.id == t.id && x.direction == t.direction)
        {
            *x = t;
        } else {
            v.push(t);
            let n = v.len();
            if n > 12 {
                v.drain(..n - 12);
            }
        }
        drop(v);
        self.publish();
    }

    fn transfer_progress(&self, id: u64, dir: &str, bytes: u64) {
        let mut done = false;
        {
            let mut v = self.transfers.lock().unwrap();
            if let Some(x) = v.iter_mut().find(|x| x.id == id && x.direction == dir) {
                x.bytes = bytes;
                x.state = "active".into();
                done = bytes >= x.total;
            }
        }
        // One chunk is 64 KB: at LAN speed that is thousands of updates a second, each
        // one a full snapshot. The UI shows ~12 a second anyway.
        static LAST_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let now = START.get_or_init(Instant::now).elapsed().as_millis() as u64;
        let last = LAST_MS.load(std::sync::atomic::Ordering::Relaxed);
        if done || now.saturating_sub(last) >= 60 {
            LAST_MS.store(now, std::sync::atomic::Ordering::Relaxed);
            self.publish();
        }
    }

    fn remove_transfer(&self, id: u64, dir: &str) {
        self.transfers
            .lock()
            .unwrap()
            .retain(|x| !(x.id == id && x.direction == dir));
        self.publish();
    }

    fn transfer_finish(
        &self,
        id: u64,
        dir: &str,
        result: std::result::Result<Option<String>, String>,
    ) {
        let mut v = self.transfers.lock().unwrap();
        if let Some(x) = v.iter_mut().find(|x| x.id == id && x.direction == dir) {
            match result {
                Ok(path) => {
                    x.state = "done".into();
                    x.bytes = x.total;
                    x.path = path;
                }
                Err(m) => {
                    x.state = "error".into();
                    x.message = m;
                }
            }
        }
        drop(v);
        self.publish();
    }
}

// ---------------------------------------------------------------- public handle

#[derive(Clone)]
pub struct Node {
    sh: Arc<Shared>,
    ev: UnboundedSender<Ev>,
}

enum Cmd {
    Connect {
        addr: IpAddr,
        port: u16,
        code: String,
    },
    ConnectKnown {
        peer_id: String,
    },
    /// One computer, or all of them.
    Disconnect(Option<String>),
    SetSide {
        peer_id: String,
        side: Edge,
    },
    Forget(String),
    SendFiles {
        paths: Vec<PathBuf>,
        target: Option<String>,
    },
    SetClipboardSync(bool),
    SetGameMode(bool),
    SetSwitchDelay(u32),
    SetDragFiles(bool),
    SetMoveApps(bool),
    SetMonitorMode(bool),
    Undo,
    BringBack,
    SetLicensePass(Option<String>),
    Power {
        peer_id: String,
        action: PowerAction,
    },
    Wake(String),
    TakeApps(String),
    SetName(String),
    NewCode,
    Engage(Option<String>),
}

impl Node {
    pub async fn start(
        cfg: NodeConfig,
        platform: Arc<dyn Platform>,
        store: Arc<dyn Store>,
    ) -> Result<Node> {
        let persisted = store.load().unwrap_or_else(|| {
            let mut id = [0u8; 8];
            rand::thread_rng().fill(&mut id);
            Persisted {
                device_id: hex(&id),
                device_name: cfg.default_name.clone(),
                peers: vec![],
                clipboard_sync: true,
                auto_file_limit_mb: 512,
                game_mode: true,
                switch_delay_ms: 0,
                drag_files: true,
                move_apps: false,
                monitor_mode: false,
            }
        });
        store.save(&persisted);

        // Bind the TCP listener (walk up a few ports if the default is busy).
        let mut listener = None;
        for off in 0..10u16 {
            if let Ok(l) = TcpListener::bind(("0.0.0.0", cfg.tcp_port.wrapping_add(off))).await {
                listener = Some(l);
                break;
            }
        }
        let listener =
            listener.ok_or_else(|| Error::Io(std::io::Error::other("no free TCP port")))?;
        let port = listener.local_addr()?.port();

        // Old clipboard-file leftovers from a previous run.
        if let Ok(rd) = std::fs::read_dir(&cfg.temp_dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("clip-") {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }

        let me = Identity {
            id: persisted.device_id.clone(),
            name: persisted.device_name.clone(),
            listen_port: port,
        };
        let (snap_tx, _) = watch::channel(Snapshot::default());
        let no_licence_needed = cfg.license_key.is_none();
        let sh = Arc::new(Shared {
            me: me.clone(),
            cfg: cfg.clone(),
            platform: platform.clone(),
            store,
            persisted: Mutex::new(persisted),
            pairing: Mutex::new(PairingState {
                code: new_code(),
                attempts_left: 3,
            }),
            discovered: Mutex::new(HashMap::new()),
            offers: Mutex::new(VecDeque::new()),
            transfers: Mutex::new(vec![]),
            status: Mutex::new(Status {
                control: "local",
                licensed: no_licence_needed,
                ..Default::default()
            }),
            devices: Mutex::new(Devices::default()),
            next_sid: AtomicU64::new(1),
            snap_tx,
        });
        sh.publish();

        let (ev_tx, ev_rx) = unbounded_channel::<Ev>();

        // Platform capture events -> actor.
        let (cap_tx, mut cap_rx) = unbounded_channel::<CaptureEvent>();
        {
            let p = sh.persisted.lock().unwrap();
            platform.set_game_mode(p.game_mode);
            platform.set_switch_delay(p.switch_delay_ms);
            platform.set_drag_files(p.drag_files);
            platform.set_move_apps(p.spans());
        }
        platform.set_capture_sink(cap_tx);
        {
            let ev_tx = ev_tx.clone();
            tokio::spawn(async move {
                while let Some(c) = cap_rx.recv().await {
                    if ev_tx.send(Ev::Capture(c)).is_err() {
                        break;
                    }
                }
            });
        }

        // Accept loop.
        {
            let sh = sh.clone();
            let ev_tx = ev_tx.clone();
            let gate = Arc::new(Semaphore::new(8));
            tokio::spawn(async move {
                loop {
                    let Ok((stream, addr)) = listener.accept().await else {
                        continue;
                    };
                    let Ok(permit) = gate.clone().try_acquire_owned() else {
                        continue;
                    };
                    let (sh, ev_tx) = (sh.clone(), ev_tx.clone());
                    tokio::spawn(async move {
                        let _permit = permit;
                        handle_incoming(sh, ev_tx, stream, addr).await;
                    });
                }
            });
        }

        // Discovery.
        if cfg.discovery {
            tokio::spawn(discovery::run_beacon(
                me.id.clone(),
                me.name.clone(),
                port,
                cfg.udp_port,
            ));
            let (d_tx, mut d_rx) = unbounded_channel();
            tokio::spawn(discovery::run_listener(me.id.clone(), cfg.udp_port, d_tx));
            let sh2 = sh.clone();
            tokio::spawn(async move {
                while let Some(d) = d_rx.recv().await {
                    sh2.discovered
                        .lock()
                        .unwrap()
                        .insert(d.id.clone(), (d, Instant::now()));
                    sh2.publish();
                }
            });
        }

        let actor = Actor {
            sh: sh.clone(),
            ev_tx: ev_tx.clone(),
            control: Control::Local,
            sessions: HashMap::new(),
            dialing: HashMap::new(),
            last_dial: HashMap::new(),
            no_auto: HashMap::new(),
            last_peer: None,
            last_clip_sig: None,
            clip_written_at: None,
            last_dial_err: None,
            my_pass: None,
            carry: None,
            drops: HashMap::new(),
            early_release: None,
            held: None,
            called_off: HashSet::new(),
            holding_local: false,
            left_down: false,
            just_left: None,
            pending_app: HashMap::new(),
            charge: Default::default(),
            started: Instant::now(),
            ticks: 0,
            sent_info: DeviceInfo::default(),
            sent_info_at: Instant::now(),
            span_out: None,
            span_driving: false,
            span_in: None,
            mirrors_out: HashMap::new(),
            mirrors_in: HashMap::new(),
            mirror_k: HashMap::new(),
            span_view: None,
            span_drop: None,
            portal: None,
            last_move: None,
            received: VecDeque::new(),
            carry_from: HashMap::new(),
            undone: HashSet::new(),
            clip: clipboard_worker(sh.platform.clone(), ev_tx.clone()),
        };
        tokio::spawn(actor.run(ev_rx));
        Ok(Node { sh, ev: ev_tx })
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.sh.snap_tx.subscribe()
    }
    pub fn snapshot(&self) -> Snapshot {
        self.sh.snapshot()
    }
    pub fn tcp_port(&self) -> u16 {
        self.sh.me.listen_port
    }
    fn cmd(&self, c: Cmd) {
        let _ = self.ev.send(Ev::Cmd(c));
    }
    /// Pair with a computer by typing the code it shows.
    pub fn connect(&self, addr: IpAddr, port: u16, code: String) {
        self.cmd(Cmd::Connect { addr, port, code })
    }
    /// Reconnect to an already-paired computer.
    pub fn connect_known(&self, peer_id: String) {
        self.cmd(Cmd::ConnectKnown { peer_id })
    }
    /// Disconnect from every computer.
    pub fn disconnect(&self) {
        self.cmd(Cmd::Disconnect(None))
    }
    /// Disconnect from one computer.
    pub fn disconnect_peer(&self, peer_id: String) {
        self.cmd(Cmd::Disconnect(Some(peer_id)))
    }
    pub fn set_side(&self, peer_id: String, side: Edge) {
        self.cmd(Cmd::SetSide { peer_id, side })
    }
    pub fn forget(&self, peer_id: String) {
        self.cmd(Cmd::Forget(peer_id))
    }
    /// Send files to the computer being controlled, or to every connected computer.
    pub fn send_files(&self, paths: Vec<PathBuf>) {
        self.cmd(Cmd::SendFiles {
            paths,
            target: None,
        })
    }
    /// Send files to one connected computer.
    pub fn send_files_to(&self, paths: Vec<PathBuf>, peer_id: String) {
        self.cmd(Cmd::SendFiles {
            paths,
            target: Some(peer_id),
        })
    }
    pub fn set_clipboard_sync(&self, on: bool) {
        self.cmd(Cmd::SetClipboardSync(on))
    }
    pub fn set_game_mode(&self, on: bool) {
        self.cmd(Cmd::SetGameMode(on))
    }
    /// Undo the last move (the bar's undo button).
    pub fn undo(&self) {
        self.cmd(Cmd::Undo);
    }
    /// Second-monitor mode: every window shown across goes back to its own computer.
    pub fn bring_windows_back(&self) {
        self.cmd(Cmd::BringBack);
    }
    pub fn set_monitor_mode(&self, on: bool) {
        self.cmd(Cmd::SetMonitorMode(on));
    }
    pub fn set_move_apps(&self, on: bool) {
        self.cmd(Cmd::SetMoveApps(on));
    }
    /// Lock a connected computer, or put it to sleep.
    pub fn power(&self, peer_id: String, action: PowerAction) {
        self.cmd(Cmd::Power { peer_id, action });
    }
    /// Wake a sleeping computer over the network (if it allows it).
    pub fn wake(&self, peer_id: String) {
        self.cmd(Cmd::Wake(peer_id));
    }
    /// Bring every open program on that computer over here (its battery is running out).
    pub fn take_apps(&self, peer_id: String) {
        self.cmd(Cmd::TakeApps(peer_id));
    }

    /// This computer's licence pass from cubecursor.shop (or none). Sent to every
    /// connected computer; renewed by the app before it runs out.
    pub fn set_license_pass(&self, pass: Option<String>) {
        self.cmd(Cmd::SetLicensePass(pass));
    }

    pub fn set_drag_files(&self, on: bool) {
        self.cmd(Cmd::SetDragFiles(on))
    }
    pub fn set_switch_delay(&self, ms: u32) {
        self.cmd(Cmd::SetSwitchDelay(ms))
    }
    pub fn set_name(&self, name: String) {
        self.cmd(Cmd::SetName(name))
    }
    pub fn new_pairing_code(&self) {
        self.cmd(Cmd::NewCode)
    }
    /// Same as the hotkey: jump to the other computer now.
    pub fn engage(&self) {
        self.cmd(Cmd::Engage(None))
    }
    /// Jump to a particular connected computer.
    pub fn engage_peer(&self, peer_id: String) {
        self.cmd(Cmd::Engage(Some(peer_id)))
    }
}

// ---------------------------------------------------------------- connections

enum Ev {
    Capture(CaptureEvent),
    Up {
        sid: u64,
        peer: PeerInfo,
        addr: IpAddr,
        tx: UnboundedSender<Msg>,
        reader: AbortHandle,
        dialer: bool,
    },
    Msg(u64, Msg),
    Closed(u64),
    Cmd(Cmd),
    OfferBuilt {
        id: u64,
        kind: OfferKind,
        target: String,
        res: std::io::Result<Offer>,
    },
    ClipFiles(Vec<PathBuf>),
    /// The person was asked about a dragged window before a hand-off.
    /// `Err("")` means they said no.
    AppPicked {
        to: String,
        res: std::result::Result<AppRef, String>,
    },
    /// A `MoveApp` we got was acted on (the program opened or not).
    AppMoved {
        from: String,
        id: u64,
        name: String,
        ok: bool,
        note: String,
    },
    /// Carried files finished arriving (into a staging folder).
    CarryFetched {
        id: u64,
        paths: Vec<PathBuf>,
    },
    /// Carried files were moved to where the user let go of them.
    CarryDropped {
        id: u64,
        dir: PathBuf,
        res: std::result::Result<Vec<PathBuf>, String>,
    },
    /// Files sent to us arrived (offer, from whom, where they are now): kept for undo.
    Received {
        id: u64,
        from: String,
        paths: Vec<PathBuf>,
    },
    /// The clipboard worker read what is on the clipboard now.
    ClipRead(ClipContent),
    DialFailed {
        ip: IpAddr,
        err: String,
    },
}

async fn handle_incoming(
    sh: Arc<Shared>,
    ev_tx: UnboundedSender<Ev>,
    stream: TcpStream,
    addr: SocketAddr,
) {
    let _ = stream.set_nodelay(true);
    let hs = tokio::time::timeout(
        Duration::from_secs(10),
        server_handshake(stream, &sh.me, &*sh),
    )
    .await;
    let (peer, (r, w)) = match hs {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            log::info!("incoming from {addr} refused: {e}");
            return;
        }
        Err(_) => return,
    };
    match peer.purpose {
        Purpose::Control => establish(sh, ev_tx, peer, addr.ip(), (r, w), false),
        Purpose::Files => {
            // File connections only for devices that are already paired.
            if peer.new_link_key.is_some() {
                return;
            }
            let (mut r, mut w) = (r, w);
            let sh2 = sh.clone();
            let lookup = move |id: u64| {
                sh2.offers
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map(|(_, o)| o.clone())
            };
            let sh3 = sh.clone();
            let prog = move |id: u64, n: u64| sh3.transfer_progress(id, "out", n);
            let (id, res) = files::serve(&mut r, &mut w, lookup, &prog).await;
            if let Some(id) = id {
                sh.transfer_finish(id, "out", res.map(|_| None).map_err(|e| e.to_string()));
            }
        }
    }
}

fn establish(
    sh: Arc<Shared>,
    ev_tx: UnboundedSender<Ev>,
    peer: PeerInfo,
    addr: IpAddr,
    halves: Secure<TcpStream>,
    dialer: bool,
) {
    let sid = sh.next_sid.fetch_add(1, Ordering::SeqCst);
    let (mut r, w) = halves;
    let (tx, rx) = unbounded_channel::<Msg>();
    tokio::spawn(writer_task(w, rx));
    let ev2 = ev_tx.clone();
    let reader = tokio::spawn(async move {
        loop {
            match r.recv::<Msg>().await {
                Ok(m) => {
                    if ev2.send(Ev::Msg(sid, m)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    log::info!("session {sid} ended: {e}");
                    let _ = ev2.send(Ev::Closed(sid));
                    return;
                }
            }
        }
    })
    .abort_handle();
    let _ = ev_tx.send(Ev::Up {
        sid,
        peer,
        addr,
        tx,
        reader,
        dialer,
    });
}

/// Sends queued messages, merging consecutive mouse motion so a busy mouse
/// never builds up lag behind a slow link.
async fn writer_task<W: tokio::io::AsyncWrite + Unpin>(
    mut w: SecureWriter<W>,
    mut rx: UnboundedReceiver<Msg>,
) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(m) = rx.try_recv() {
            batch.push(m);
        }
        let mut out: Vec<Msg> = Vec::with_capacity(batch.len());
        for m in batch {
            match (out.last_mut(), &m) {
                (
                    Some(Msg::Input(InputEvent::MouseMove { dx, dy })),
                    Msg::Input(InputEvent::MouseMove { dx: a, dy: b }),
                ) => {
                    *dx = dx.saturating_add(*a);
                    *dy = dy.saturating_add(*b);
                }
                (
                    Some(Msg::Input(InputEvent::Wheel { dx, dy })),
                    Msg::Input(InputEvent::Wheel { dx: a, dy: b }),
                ) => {
                    *dx = dx.saturating_add(*a);
                    *dy = dy.saturating_add(*b);
                }
                _ => out.push(m),
            }
        }
        if w.send_many(&out).await.is_err() {
            return;
        }
    }
}

async fn dial(
    sh: Arc<Shared>,
    ev_tx: UnboundedSender<Ev>,
    addr: SocketAddr,
    auth: ClientAuth,
) -> Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connection timed out",
            ))
        })??;
    stream.set_nodelay(true)?;
    let (peer, halves) = tokio::time::timeout(
        Duration::from_secs(10),
        client_handshake(stream, &sh.me, Purpose::Control, auth),
    )
    .await
    .map_err(|_| Error::Auth("handshake timed out".into()))??;
    establish(sh, ev_tx, peer, addr.ip(), halves, true);
    Ok(())
}

// ---------------------------------------------------------------- actor

#[derive(Clone, Debug, PartialEq)]
enum Control {
    Local,
    /// We are swallowing local input and forwarding it to `peer`, through `edge`.
    Remote {
        peer: String,
        edge: Edge,
    },
    /// `peer` controls our cursor; `ret` is our edge that leads back to it.
    Controlled {
        peer: String,
        ret: Edge,
    },
}

/// How many computers can be connected at once: one per side and one per corner.
const MAX_PEERS: usize = 8;
/// Sides handed out to new computers, in this order.
const SIDE_ORDER: [Edge; 8] = [
    Edge::Right,
    Edge::Left,
    Edge::Top,
    Edge::Bottom,
    Edge::TopRight,
    Edge::TopLeft,
    Edge::BottomRight,
    Edge::BottomLeft,
];
/// An existing connection that has been silent this long is considered dead, and a fresh
/// connection from the same computer replaces it.
const STALE_AFTER: Duration = Duration::from_secs(2);

struct Session {
    sid: u64,
    peer: PeerInfo,
    addr: IpAddr,
    tx: UnboundedSender<Msg>,
    reader: AbortHandle,
    last_rx: Instant,
    /// Which edge of *my* screen leads to this computer.
    side: Edge,
    /// When the connection came up, and what its licence pass said (if it sent one).
    since: Instant,
    peer_licensed: bool,
    license_seen: bool,
    /// When the last ping went out (for the round trip).
    ping_at: Option<Instant>,
    /// Its screen's size in pixels, once it said.
    screen: Option<(i32, i32)>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

struct Actor {
    sh: Arc<Shared>,
    ev_tx: UnboundedSender<Ev>,
    control: Control,
    /// Connected computers by id, at most one per side of my screen.
    sessions: HashMap<String, Session>,
    /// Dials in flight, by address.
    dialing: HashMap<IpAddr, Instant>,
    last_dial: HashMap<String, Instant>,
    /// After a deliberate disconnect (either side) nobody redials that computer for a while.
    no_auto: HashMap<String, Instant>,
    /// The computer the cursor last went to: what Scroll Lock jumps to.
    last_peer: Option<String>,
    last_clip_sig: Option<u64>,
    /// When we last put something on this computer's clipboard ourselves: the change
    /// that write causes is our own echo, not something the person copied.
    clip_written_at: Option<Instant>,
    last_dial_err: Option<String>,
    /// This computer's licence pass, as given by the app.
    my_pass: Option<String>,
    /// Files being carried to us while the other computer's mouse holds them.
    carry: Option<Carry>,
    /// Carries that were let go before they finished arriving: id -> folder to drop in.
    drops: HashMap<u64, PathBuf>,
    /// The button went up before the offer for the files reached us (folder under the
    /// cursor at that moment); applies to the offer that follows in the same crossing.
    early_release: Option<PathBuf>,
    /// Our own files that went across, while the other computer still has the cursor.
    held: Option<Held>,
    /// Offers called off before their file list was even built.
    called_off: HashSet<u64>,
    /// The cursor came back with `held` still in hand: it shows under the cursor again.
    holding_local: bool,
    /// Whether the left button is down, as far as the input we send or receive says.
    left_down: bool,
    /// The computer whose cursor just handed control back to us, and when.
    just_left: Option<(String, Instant)>,
    /// The program we sent across by dragging its window: id, computer, program.
    pending_app: HashMap<u64, (String, AppRef)>,
    /// "Full in …" for this computer's battery.
    charge: crate::status::ChargeClock,
    started: Instant,
    ticks: u64,
    /// What we last told the other computers about how we are doing, and when.
    sent_info: DeviceInfo,
    sent_info_at: Instant,
    /// A window of ours hanging across an edge: the computer on that side, and the program.
    span_out: Option<String>,
    /// The other computer's mouse is dragging one of our windows across (we are controlled).
    span_driving: bool,
    /// A window another computer is dragging over to us: that computer, and its size.
    span_in: Option<(String, u32, u32)>,
    /// Second-monitor mode: our windows shown on another computer (id → that computer).
    mirrors_out: HashMap<u64, String>,
    /// Other computers' windows shown here (id → that computer).
    mirrors_in: HashMap<u64, String>,
    /// How much bigger each of those is shown here than it really is (screens of other
    /// resolutions): positions on it are scaled by this both ways.
    mirror_k: HashMap<u64, f32>,
    /// Where the window coming over was last shown here (scaled to this screen).
    span_view: Option<SpanView>,
    /// A window was just let go over here: the pointer comes in where it held it.
    span_drop: Option<(String, (i32, i32), Instant)>,
    /// The pointer is on a mirrored window.
    portal: Option<Portal>,
    /// The last move, for the undo button.
    last_move: Option<(Undo, Instant)>,
    /// Files that arrived here, by offer: who sent them and where they are (for undo).
    received: VecDeque<(u64, String, Vec<PathBuf>)>,
    /// Carried files on their way here: who carries them.
    carry_from: HashMap<u64, String>,
    /// Offers undone before their files had arrived: they go to the bin when they do.
    undone: HashSet<u64>,
    /// Clipboard reads / writes run on their own thread: the clipboard can be held by
    /// another app for a moment and a big image takes a while to copy, and the actor
    /// that forwards mouse movement must never wait for either.
    clip: std::sync::mpsc::Sender<ClipJob>,
}

/// How long the last move can be undone.
const UNDO_FOR: Duration = Duration::from_secs(120);

/// A move that can be undone.
#[derive(Clone, Debug)]
enum Undo {
    /// A program opened on `peer` (and maybe closed here).
    App { peer: String, app: AppRef },
    /// Files sent to `peer` (offer `id`).
    Files { peer: String, id: u64 },
    /// A window of ours shown on `peer`.
    Mirror { peer: String, id: u64 },
}

/// The pointer is on a mirrored window (second-monitor mode).
#[derive(Clone, Debug, PartialEq)]
struct Portal {
    /// The other computer (where the window is shown, or whose window it is).
    peer: String,
    id: u64,
    /// The window is ours: the pointer is really here, on the real window.
    owner: bool,
    /// The last place of the pointer inside the window.
    at: (i32, i32),
}

/// Files a computer is carrying over with its mouse, waiting for the button to be let go.
/// Files picked up on this computer and taken across the edge: if the cursor comes back
/// with the button still held, they stay in hand and are dropped here.
struct Held {
    peer: String,
    /// The offer that carries these files (cancelled when the cursor comes back).
    offer: u64,
    paths: Vec<PathBuf>,
    view: CarryView,
}

struct Carry {
    id: u64,
    peer: String,
    view: CarryView,
    /// Set once everything has arrived (in a staging folder).
    staged: Option<Vec<PathBuf>>,
    /// The download, so it can be stopped if the cursor goes back.
    fetch: Option<tokio::task::AbortHandle>,
    /// Where it is being downloaded to.
    dir: Option<PathBuf>,
}

enum ClipJob {
    Read,
    Write(ClipContent),
}

fn clipboard_worker(
    plat: Arc<dyn Platform>,
    ev_tx: UnboundedSender<Ev>,
) -> std::sync::mpsc::Sender<ClipJob> {
    let (tx, rx) = std::sync::mpsc::channel::<ClipJob>();
    let _ = std::thread::Builder::new()
        .name("tandem-clip".into())
        .spawn(move || {
            while let Ok(mut job) = rx.recv() {
                // Several change notifications in a row only need the latest read, but
                // writes always run, in order.
                if matches!(job, ClipJob::Read) {
                    while let Ok(next) = rx.try_recv() {
                        if let ClipJob::Write(_) = next {
                            job = next;
                            break;
                        }
                    }
                }
                match job {
                    ClipJob::Read => {
                        if let Some(c) = plat.clipboard_get() {
                            if ev_tx.send(Ev::ClipRead(c)).is_err() {
                                break;
                            }
                        }
                    }
                    ClipJob::Write(c) => plat.clipboard_set(&c),
                }
            }
        });
    tx
}

fn sig(c: &ClipContent) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match c {
        ClipContent::Text(t) => {
            0u8.hash(&mut h);
            t.hash(&mut h)
        }
        ClipContent::Files(f) => {
            1u8.hash(&mut h);
            f.hash(&mut h)
        }
        ClipContent::Image(b) => {
            2u8.hash(&mut h);
            b.hash(&mut h)
        }
    }
    h.finish()
}

/// Where the cursor appears when it enters through `edge` (2 px inside).
pub fn edge_point(s: Screen, edge: Edge, frac: f32) -> (i32, i32) {
    let f = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let x = s.x + ((s.width - 1) as f32 * f) as i32;
    let y = s.y + ((s.height - 1) as f32 * f) as i32;
    match edge {
        Edge::Left => (s.x + 2, y),
        Edge::Right => (s.x + s.width - 3, y),
        Edge::Top => (x, s.y + 2),
        Edge::Bottom => (x, s.y + s.height - 3),
        Edge::TopLeft => (s.x + 2, s.y + 2),
        Edge::TopRight => (s.x + s.width - 3, s.y + 2),
        Edge::BottomLeft => (s.x + 2, s.y + s.height - 3),
        Edge::BottomRight => (s.x + s.width - 3, s.y + s.height - 3),
    }
}

impl Actor {
    async fn run(mut self, mut rx: UnboundedReceiver<Ev>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                Some(ev) = rx.recv() => self.on_event(ev),
                _ = tick.tick() => self.on_tick(),
                else => break,
            }
        }
    }

    fn set_control(&mut self, c: Control) {
        if !matches!(c, Control::Controlled { .. }) {
            self.cancel_carry();
            self.early_release = None;
        }
        let peer = match &c {
            Control::Local => None,
            Control::Remote { peer, .. } | Control::Controlled { peer, .. } => Some(peer.clone()),
        };
        let label = match &c {
            Control::Local => "local",
            Control::Remote { .. } => "remote",
            Control::Controlled { .. } => "controlled",
        };
        files::SLOW_DOWN.store(c != Control::Local, std::sync::atomic::Ordering::Relaxed);
        self.control = c;
        {
            let mut st = self.sh.status.lock().unwrap();
            st.control = label;
            st.control_peer = peer;
        }
        self.sh.publish();
    }

    fn send_to(&self, peer: &str, m: Msg) {
        if let Some(s) = self.sessions.get(peer) {
            let _ = s.tx.send(m);
        }
    }

    /// Send to every connected computer except `except` (if given).
    fn send_all_but(&self, except: Option<&str>, m: &Msg) {
        for (id, s) in &self.sessions {
            if Some(id.as_str()) != except {
                let _ = s.tx.send(m.clone());
            }
        }
    }

    fn center(&self) -> (i32, i32) {
        self.sh.platform.screen().center()
    }

    /// Recomputes what the UI shows as connected and tells the platform which screen
    /// edges lead to a computer.
    fn refresh_connected(&mut self) {
        let mut v: Vec<PeerBrief> = self
            .sessions
            .values()
            .map(|s| PeerBrief {
                id: s.peer.id.clone(),
                name: s.peer.name.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        self.sh.status.lock().unwrap().connected = v;
        let edges: Vec<Edge> = self.sessions.values().map(|s| s.side).collect();
        self.sh.platform.set_active_edges(&edges);
        self.sh.publish();
    }

    // ---- licence

    /// Our pass, if it is valid for this computer right now.
    fn my_pass_if_valid(&self) -> Option<String> {
        let key = self.sh.cfg.license_key?;
        let t = self.my_pass.as_ref()?;
        let p = crate::license::verify(t, &key, crate::license::now()).ok()?;
        (p.dev == self.sh.me.id).then(|| t.clone())
    }

    /// A pass a connected computer sent: valid, and really that computer's.
    fn peer_pass_ok(&self, from: &str, pass: Option<&str>) -> bool {
        let (Some(key), Some(t)) = (self.sh.cfg.license_key, pass) else {
            return false;
        };
        crate::license::verify(t, &key, crate::license::now())
            .map(|p| p.dev == from)
            .unwrap_or(false)
    }

    /// Our pass came, changed or ran out: tell everyone, and check every connection.
    fn license_changed(&mut self) {
        let pass = self.my_pass_if_valid();
        let licensed = pass.is_some() || self.sh.cfg.license_key.is_none();
        self.sh.status.lock().unwrap().licensed = licensed;
        for s in self.sessions.values() {
            let _ = s.tx.send(Msg::License { pass: pass.clone() });
        }
        self.enforce_license();
        self.sh.publish();
    }

    /// Two computers stay connected only when at least one of them is licensed. The
    /// other side gets a few seconds to show its pass first.
    fn enforce_license(&mut self) {
        if self.sh.cfg.license_key.is_none() || self.my_pass_if_valid().is_some() {
            return;
        }
        let grace = Duration::from_secs(6);
        let out: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| !s.peer_licensed && (s.license_seen || s.since.elapsed() > grace))
            .map(|(id, _)| id.clone())
            .collect();
        for id in out {
            log::info!("licence: neither computer is licensed, disconnecting");
            // Don't redial straight away: nothing would change.
            self.no_auto
                .insert(id.clone(), Instant::now() + Duration::from_secs(120));
            self.drop_session(
                &id,
                "Needs a CubeCursor licence on one of the two computers",
            );
        }
    }

    /// Ends one connection, handing the mouse back if it was involved.
    /// How this computer is doing: kept for the bar, and sent to the others when it
    /// changed (or every 15 seconds, for the load).
    fn read_device(&mut self) {
        let mut info = self.sh.platform.device_info();
        let t = self.started.elapsed().as_secs_f64();
        info.mins_to_full = self.charge.update(t, info.battery, info.charging);
        let changed = {
            let mut d = self.sh.devices.lock().unwrap();
            let c = d.me != info;
            d.me = info.clone();
            c
        };
        let due = crate::status::worth_sending_now(&self.sent_info, &info)
            || self.sent_info_at.elapsed() >= Duration::from_secs(15);
        if due && !self.sessions.is_empty() {
            self.send_all_but(None, &Msg::Status(info.clone()));
            self.sent_info = info;
            self.sent_info_at = Instant::now();
        }
        if changed {
            self.sh.publish();
        }
    }

    /// The soft glow where the cursor crosses, in the colour of the computer it goes to.
    fn glow(&self, edge: Edge, frac: f32, to: &str) {
        self.sh.platform.edge_glow(edge, frac, device_rgb(to));
    }

    fn drop_session(&mut self, pid: &str, why: &str) {
        let Some(sess) = self.sessions.remove(pid) else {
            return;
        };
        self.sh.devices.lock().unwrap().peers.remove(pid);
        if self.span_in.as_ref().is_some_and(|(p, ..)| p == pid) {
            self.span_in = None;
            self.sh.platform.span_hide(0);
        }
        if self.portal.as_ref().is_some_and(|p| p.peer == pid) {
            self.clear_portal();
        }
        for (id, _) in self
            .mirrors_out
            .clone()
            .iter()
            .filter(|(_, p)| p.as_str() == pid)
        {
            self.mirrors_out.remove(id);
            self.sh.platform.mirror_release(*id, None);
        }
        for (id, _) in self
            .mirrors_in
            .clone()
            .iter()
            .filter(|(_, p)| p.as_str() == pid)
        {
            self.mirrors_in.remove(id);
            self.sh.platform.mirror_close(*id);
        }
        if self.span_out.as_deref() == Some(pid) {
            self.span_out = None;
            self.span_driving = false;
            self.sh.platform.span_cancel();
        }
        match &self.control {
            Control::Remote { peer, .. } if peer == pid => {
                let (x, y) = self.center();
                self.sh.platform.end_capture(x, y);
                self.set_control(Control::Local);
            }
            Control::Controlled { peer, .. } if peer == pid => {
                self.sh.platform.release_all();
                self.set_control(Control::Local);
            }
            _ => {}
        }
        drop(sess);
        if !why.is_empty() {
            self.sh.status.lock().unwrap().error = Some(why.to_string());
        }
        self.refresh_connected();
    }

    fn on_tick(&mut self) {
        self.ticks += 1;
        self.sync_windows();
        if self.ticks % 3 == 1 {
            self.read_device();
        }
        let mut dead = Vec::new();
        for (id, s) in self.sessions.iter_mut() {
            let _ = s.tx.send(Msg::Ping);
            if s.ping_at.is_none() {
                s.ping_at = Some(Instant::now());
            }
            // While the mouse is on the other computer (or theirs is here), a dead link
            // is noticed sooner, so the mouse comes back instead of feeling frozen.
            let busy = matches!(&self.control,
                Control::Remote { peer, .. } | Control::Controlled { peer, .. } if peer == id);
            let limit = if busy {
                SESSION_TIMEOUT_BUSY
            } else {
                SESSION_TIMEOUT
            };
            if s.last_rx.elapsed() > limit {
                dead.push(id.clone());
            }
        }
        for id in dead {
            self.drop_session(&id, "connection lost");
        }
        // A pass running out while the app is offline: stop being a main.
        let licensed = self.my_pass_if_valid().is_some() || self.sh.cfg.license_key.is_none();
        if licensed != self.sh.status.lock().unwrap().licensed {
            self.license_changed();
        }
        self.enforce_license();
        self.dialing
            .retain(|_, t| t.elapsed() <= Duration::from_secs(20));
        self.no_auto.retain(|_, t| Instant::now() < *t);
        {
            let mut st = self.sh.status.lock().unwrap();
            if st
                .notice
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(12))
            {
                st.notice = None;
            }
        }
        self.auto_connect();
        self.sh.publish(); // lets "nearby" entries expire
    }

    /// Reconnect to paired computers. Of two computers only the one with the smaller id
    /// dials, so they don't dial each other at the same time.
    fn auto_connect(&mut self) {
        let per = self.sh.persisted.lock().unwrap().clone();
        for p in &per.peers {
            if self.sessions.len() >= MAX_PEERS {
                return;
            }
            if self.sh.me.id >= p.id || self.sessions.contains_key(&p.id) {
                continue;
            }
            if self.no_auto.contains_key(&p.id) {
                continue;
            }
            if self
                .last_dial
                .get(&p.id)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(4))
            {
                continue;
            }
            let disc = self.sh.discovered.lock().unwrap().get(&p.id).cloned();
            let target = match disc {
                Some((d, seen)) if seen.elapsed() < NEARBY_TTL => Some((d.addr, d.port)),
                _ => p
                    .last_addr
                    .as_ref()
                    .and_then(|a| a.parse::<SocketAddr>().ok())
                    .map(|a| (a.ip(), a.port())),
            };
            if let (Some((ip, port)), Some(key)) = (target, unhex32(&p.link_key)) {
                if self.dialing.contains_key(&ip) {
                    continue;
                }
                self.last_dial.insert(p.id.clone(), Instant::now());
                self.start_dial(
                    SocketAddr::new(ip, port),
                    ClientAuth::Known { link_key: key },
                );
            }
        }
    }

    fn start_dial(&mut self, addr: SocketAddr, auth: ClientAuth) {
        self.dialing.insert(addr.ip(), Instant::now());
        let (sh, ev_tx) = (self.sh.clone(), self.ev_tx.clone());
        tokio::spawn(async move {
            if let Err(e) = dial(sh, ev_tx.clone(), addr, auth).await {
                let _ = ev_tx.send(Ev::DialFailed {
                    ip: addr.ip(),
                    err: e.to_string(),
                });
            }
        });
    }

    fn on_event(&mut self, ev: Ev) {
        match ev {
            Ev::Capture(c) => self.on_capture(c),
            Ev::Up {
                sid,
                peer,
                addr,
                tx,
                reader,
                dialer,
            } => self.on_up(sid, peer, addr, tx, reader, dialer),
            Ev::Msg(sid, m) => {
                let found = self
                    .sessions
                    .iter_mut()
                    .find(|(_, s)| s.sid == sid)
                    .map(|(id, s)| {
                        s.last_rx = Instant::now();
                        id.clone()
                    });
                if let Some(pid) = found {
                    self.on_msg(&pid, m);
                }
            }
            Ev::Closed(sid) => {
                let found = self
                    .sessions
                    .iter()
                    .find(|(_, s)| s.sid == sid)
                    .map(|(id, _)| id.clone());
                if let Some(pid) = found {
                    self.drop_session(&pid, "disconnected");
                }
            }
            Ev::Cmd(c) => self.on_cmd(c),
            Ev::DialFailed { ip, err } => {
                self.dialing.remove(&ip);
                // Quiet during background reconnects; loud only when nothing else is going on.
                // Once per kind of failure, not every few seconds while the other PC is off.
                if self.last_dial_err.as_deref() != Some(err.as_str()) {
                    log::info!("dial failed: {err}");
                    self.last_dial_err = Some(err.clone());
                }
                self.sh.set_error(Some(err));
            }
            Ev::OfferBuilt {
                id,
                kind,
                target,
                res,
            } => self.on_offer_built(id, kind, target, res),
            Ev::CarryFetched { id, paths } => self.on_carry_fetched(id, paths),
            Ev::CarryDropped { id, dir, res } => match res {
                Ok(paths) => {
                    if let Some(from) = self.carry_from.remove(&id) {
                        self.received_files(id, from, paths);
                    }
                    let name = dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.notify(&format!("Dropped into {name}"));
                    self.sh
                        .transfer_finish(id, "in", Ok(Some(dir.to_string_lossy().into_owned())))
                }
                Err(e) => self.sh.transfer_finish(id, "in", Err(e)),
            },
            Ev::Received { id, from, paths } => self.received_files(id, from, paths),
            Ev::ClipFiles(paths) => {
                let c = ClipContent::Files(paths);
                self.last_clip_sig = Some(sig(&c));
                let _ = self.clip.send(ClipJob::Write(c));
            }
            Ev::ClipRead(c) => self.on_clip_read(c),
            Ev::AppPicked { to, res } => match res {
                Ok(app) => {
                    // The computer may have gone away while the question was open.
                    if !self.sessions.contains_key(&to) {
                        self.sh.platform.span_restore();
                        return;
                    }
                    let who = self
                        .sessions
                        .get(&to)
                        .map(|s| s.peer.name.clone())
                        .unwrap_or_default();
                    let id: u64 = rand::thread_rng().gen();
                    log::info!("app move: sending {} to {who}", app.exe);
                    self.notify(&format!("Moving {} to {who}…", app.name));
                    self.pending_app.insert(id, (to.clone(), app.clone()));
                    self.send_to(&to, Msg::MoveApp { id, app });
                }
                Err(e) if e.is_empty() => log::info!("app move: the person said no"),
                Err(e) => {
                    log::info!("app move: {e}");
                    self.notify(&e)
                }
            },
            Ev::AppMoved {
                from,
                id,
                name,
                ok,
                note,
            } => {
                log::info!(
                    "app move: opening {name} here: {}",
                    if ok { "done".to_string() } else { note.clone() }
                );
                if ok {
                    self.notify(&format!("{name} opened here"));
                }
                self.send_to(&from, Msg::MoveAppAnswer { id, ok, note });
            }
        }
    }

    /// The first side not used by another paired computer.
    fn first_free_side(&self, pid: &str) -> Option<Edge> {
        let per = self.sh.persisted.lock().unwrap();
        SIDE_ORDER
            .into_iter()
            .find(|e| !per.peers.iter().any(|p| p.id != pid && p.side == *e))
    }

    fn on_up(
        &mut self,
        sid: u64,
        peer: PeerInfo,
        addr: IpAddr,
        tx: UnboundedSender<Msg>,
        reader: AbortHandle,
        dialer: bool,
    ) {
        self.dialing.remove(&addr);
        let pid = peer.id.clone();
        // The same computer again. A healthy connection stays; a silent one is dead and
        // the new connection takes its place.
        match self.sessions.get(&pid) {
            Some(old) if old.last_rx.elapsed() < STALE_AFTER => {
                reader.abort();
                return;
            }
            Some(_) => self.drop_session(&pid, ""),
            None => {}
        }
        if self.sessions.len() >= MAX_PEERS {
            reader.abort();
            self.sh.set_error(Some(
                "Four computers are already connected — disconnect one first".into(),
            ));
            return;
        }
        // Persist a new pairing / refresh name & address.
        let known = self
            .sh
            .persisted
            .lock()
            .unwrap()
            .peers
            .iter()
            .any(|p| p.id == pid);
        let fresh_side = if known {
            None
        } else {
            self.first_free_side(&pid)
        };
        let mut side = {
            let mut per = self.sh.persisted.lock().unwrap();
            let sock = SocketAddr::new(addr, peer.listen_port).to_string();
            if let Some(p) = per.peers.iter_mut().find(|p| p.id == pid) {
                p.name = peer.name.clone();
                p.last_addr = Some(sock);
                if let Some(k) = peer.new_link_key {
                    p.link_key = hex(&k);
                }
                p.side
            } else if let Some(k) = peer.new_link_key {
                let side = fresh_side.unwrap_or(Edge::Right);
                per.peers.push(PairedPeer {
                    id: pid.clone(),
                    name: peer.name.clone(),
                    link_key: hex(&k),
                    side,
                    last_addr: Some(sock),
                    mac: None,
                });
                side
            } else {
                // Known-mode connection for a peer we no longer have: cannot happen (policy checked).
                reader.abort();
                return;
            }
        };
        // Two connected computers never share a side (with more than eight paired, the
        // stored sides can clash).
        if self.sessions.values().any(|s| s.side == side) {
            if let Some(free) = SIDE_ORDER
                .into_iter()
                .find(|e| !self.sessions.values().any(|s| s.side == *e))
            {
                side = free;
                let mut per = self.sh.persisted.lock().unwrap();
                if let Some(p) = per.peers.iter_mut().find(|p| p.id == pid) {
                    p.side = free;
                }
            }
        }
        self.sh.save();
        self.last_dial_err = None;
        self.sessions.insert(
            pid.clone(),
            Session {
                sid,
                peer,
                addr,
                tx,
                reader,
                last_rx: Instant::now(),
                side,
                since: Instant::now(),
                peer_licensed: false,
                license_seen: false,
                ping_at: None,
                screen: None,
            },
        );
        let pass = self.my_pass_if_valid();
        self.send_to(&pid, Msg::License { pass });
        let scr = self.sh.platform.screen();
        self.send_to(
            &pid,
            Msg::Screen {
                w: scr.width,
                h: scr.height,
            },
        );
        let me = self.sh.devices.lock().unwrap().me.clone();
        self.send_to(&pid, Msg::Status(me));
        self.sh.status.lock().unwrap().error = None;
        self.sh.discovered.lock().unwrap().remove(&pid);
        self.refresh_connected();
        if dialer {
            self.send_to(&pid, Msg::Layout { peer_side: side });
        }
    }

    fn on_capture(&mut self, c: CaptureEvent) {
        match c {
            CaptureEvent::Crossed { edge, frac } => {
                let target = if self.control == Control::Local {
                    self.sessions
                        .iter()
                        .find(|(_, s)| s.side == edge)
                        .map(|(id, _)| id.clone())
                } else {
                    None
                };
                if let Some(pid) = target {
                    self.last_peer = Some(pid.clone());
                    let again = self
                        .held
                        .as_ref()
                        .filter(|h| self.holding_local && h.peer == pid)
                        .map(|h| h.paths.clone());
                    self.holding_local = false;
                    self.set_control(Control::Remote {
                        peer: pid.clone(),
                        edge,
                    });
                    self.send_to(&pid, Msg::Enter { edge, frac });
                    self.glow(edge, frac, &pid);
                    if let Some(paths) = again {
                        // Still holding the files we brought back: take them over again.
                        let offer = self.build_offer(paths, OfferKind::Carry, pid);
                        if let Some(h) = self.held.as_mut() {
                            h.offer = offer;
                        }
                    }
                    self.sync_carry();
                } else {
                    // Raced with a disconnect or a layout change: give the user their mouse back.
                    let (x, y) = self.center();
                    self.sh.platform.end_capture(x, y);
                }
            }
            CaptureEvent::DragCrossed { edge, frac, paths } => {
                let target = if self.control == Control::Local
                    && self.sh.persisted.lock().unwrap().drag_files
                {
                    self.sessions
                        .iter()
                        .find(|(_, s)| s.side == edge)
                        .map(|(id, _)| id.clone())
                } else {
                    None
                };
                if let Some(pid) = target {
                    self.last_peer = Some(pid.clone());
                    self.set_control(Control::Remote {
                        peer: pid.clone(),
                        edge,
                    });
                    self.send_to(&pid, Msg::Enter { edge, frac });
                    let n = paths.len();
                    self.left_down = true;
                    let offer = self.build_offer(paths.clone(), OfferKind::Carry, pid.clone());
                    self.held = Some(Held {
                        peer: pid.clone(),
                        offer,
                        view: carry_view(&paths),
                        paths,
                    });
                    self.holding_local = false;
                    self.notify(&format!(
                        "Carrying {n} item{} — let go on the other screen to drop",
                        if n == 1 { "" } else { "s" }
                    ));
                } else {
                    let (x, y) = self.center();
                    self.sh.platform.end_capture(x, y);
                }
            }
            CaptureEvent::AppDragged { edge, app } => {
                if !self.sh.persisted.lock().unwrap().move_apps {
                    return;
                }
                log::info!("app move: {} dragged to the {edge:?} edge", app.exe);
                let Some(to) = self
                    .sessions
                    .iter()
                    .find(|(_, s)| s.side == edge)
                    .map(|(id, _)| id.clone())
                else {
                    self.notify("No computer is set on that side");
                    return;
                };
                self.offer_app(to, app);
            }
            CaptureEvent::SpanBegin { edge, w, h } => {
                let to = self
                    .sessions
                    .iter()
                    .find(|(_, s)| s.side == edge)
                    .map(|(id, _)| id.clone());
                log::info!("app span: a {w}x{h} window crosses the {edge:?} edge");
                // The pointer may drive it on across the whole other screen.
                let room = to.as_ref().and_then(|t| self.sessions.get(t)).and_then(|s| {
                    let (pw, ph) = s.screen?;
                    let mine = self.sh.platform.screen();
                    let k = crate::span::scale_for(edge, (mine.width, mine.height), (pw, ph));
                    let depth = if matches!(edge, Edge::Top | Edge::Bottom) { ph } else { pw };
                    Some((depth as f32 / k) as i32 - 1)
                });
                crate::span::set_room(room.unwrap_or(0));
                if let Some(to) = to {
                    self.send_to(&to, Msg::SpanBegin { w, h });
                    self.span_out = Some(to);
                }
            }
            CaptureEvent::SpanMove {
                crossed,
                along,
                gx,
                gy,
            } => {
                if let Some(to) = &self.span_out {
                    self.send_to(
                        to,
                        Msg::SpanPos {
                            crossed,
                            along,
                            gx,
                            gy,
                        },
                    );
                }
            }
            CaptureEvent::SpanFrame(jpeg) => {
                if let Some(to) = &self.span_out {
                    self.send_to(to, Msg::SpanFrame(jpeg));
                }
            }
            CaptureEvent::SpanEnd { moved, app, frac } => {
                self.span_driving = false;
                let Some(to) = self.span_out.take() else {
                    if moved {
                        self.sh.platform.span_restore();
                    }
                    return;
                };
                self.send_to(&to, Msg::SpanEnd { moved });
                if !moved {
                    return;
                }
                let who = self
                    .sessions
                    .get(&to)
                    .map(|s| s.peer.name.clone())
                    .unwrap_or_else(|| "the other computer".into());
                // Driven from there (we are being controlled): the mouse goes back now.
                if matches!(&self.control, Control::Controlled { peer, .. } if *peer == to) {
                    self.left_down = false;
                    self.sh.platform.release_all();
                    self.set_control(Control::Local);
                    self.send_to(&to, Msg::Leave { frac });
                }
                if self.sh.persisted.lock().unwrap().monitor_mode {
                    // Second-monitor mode: the window stays here and is shown over there.
                    let edge = self.sessions.get(&to).map(|s| s.side);
                    match edge.and_then(|e| self.sh.platform.mirror_adopt(e)) {
                        Some((id, w, h)) => {
                            log::info!("mirror: window {id} ({w}x{h}) is shown on {who} now");
                            self.mirrors_out.insert(id, to.clone());
                            self.send_to(&to, Msg::MirrorStart { id, w, h });
                            self.remember(
                                Undo::Mirror {
                                    peer: to.clone(),
                                    id,
                                },
                                format!("Window on {who}"),
                            );
                        }
                        None => {
                            self.sh.platform.span_restore();
                            self.notify(&format!("That window can't be shown on {who}"));
                        }
                    }
                    return;
                }
                let movable = app.filter(|a| a.is_sane() && !(a.is_browser() && a.url.is_none()));
                match movable {
                    Some(app) if self.sh.persisted.lock().unwrap().move_apps => {
                        log::info!("app span: let go over {who}: moving {}", app.exe);
                        // Dragged over on purpose and let go there: no question to ask.
                        let _ = self.ev_tx.send(Ev::AppPicked { to, res: Ok(app) });
                    }
                    _ => {
                        self.sh.platform.span_restore();
                        self.notify(&format!("That window can't be moved to {who}"));
                    }
                }
            }
            CaptureEvent::PortalIn { id, rx, ry } => self.portal_in_here(id, rx, ry, true),
            CaptureEvent::PortalOut { rx, ry } => self.portal_out_here(rx, ry, true),
            CaptureEvent::PortalCursor { id, rx, ry } => {
                if let Some(p) = self.portal.as_mut().filter(|p| p.owner && p.id == id) {
                    p.at = (rx, ry);
                    let peer = p.peer.clone();
                    self.send_to(&peer, Msg::PortalCursor { id, rx, ry });
                }
            }
            CaptureEvent::MirrorFrame { id, w, h, jpeg } => {
                if let Some(to) = self.mirrors_out.get(&id) {
                    self.send_to(&to.clone(), Msg::MirrorFrame { id, w, h, jpeg });
                }
            }
            CaptureEvent::MirrorMoved { id, dx, dy, done } => {
                if let Some(to) = self.mirrors_out.get(&id) {
                    self.send_to(&to.clone(), Msg::MirrorMoved { id, dx, dy, done });
                }
            }
            CaptureEvent::MirrorDismissed { id } => {
                // Closed here by the person (its taskbar button): the window goes home.
                if self.send_back(id) {
                    log::info!("mirror: window {id} closed here: it goes back");
                }
            }
            CaptureEvent::MirrorGone { id } => {
                let Some(to) = self.mirrors_out.remove(&id) else {
                    return;
                };
                log::info!("mirror: window {id} was closed");
                // The pointer was on it: it carries on over there, where it was seen.
                if let Some(p) = self.portal.clone().filter(|p| p.owner && p.id == id) {
                    match self.control.clone() {
                        Control::Local => {
                            self.sh.platform.begin_capture();
                            self.portal_out_here(p.at.0, p.at.1, false);
                        }
                        _ => self.portal_out_remote(p.at.0, p.at.1),
                    }
                }
                self.send_to(&to, Msg::MirrorEnd { id });
            }
            CaptureEvent::LocalLeftUp => {
                let was_down = std::mem::replace(&mut self.left_down, false);
                if self.holding_local {
                    self.drop_held_locally();
                } else if let Some(peer) = self.carry.as_ref().map(|c| c.peer.clone()) {
                    self.on_carry_release(&peer);
                } else if let Some((peer, at)) = self.just_left.clone() {
                    // The button went up before the files' offer reached us (only when it
                    // was held down the whole way back, not for an ordinary click).
                    if was_down && at.elapsed() < Duration::from_secs(2) {
                        self.on_carry_release(&peer);
                    }
                }
            }
            CaptureEvent::Input(ev) => {
                self.track_button(&ev);
                if let Control::Remote { peer, .. } = &self.control {
                    if let Some(s) = self.sessions.get(peer) {
                        let _ = s.tx.send(Msg::Input(ev));
                    }
                }
            }
            CaptureEvent::Failsafe => {
                self.clear_portal();
                self.cancel_outgoing();
                self.held = None;
                if let Control::Remote { peer, .. } = self.control.clone() {
                    self.set_control(Control::Local);
                    self.send_to(&peer, Msg::Release);
                }
            }
            CaptureEvent::HotkeyEngage => self.engage(None),
            CaptureEvent::ClipboardChanged => self.on_clipboard_changed(),
            CaptureEvent::InputBlocked => self.on_input_blocked(),
        }
    }

    /// A protected Windows screen (admin prompt, lock screen) took the input desktop.
    /// Nobody can type into it from here, so whichever side was driving gives the
    /// keyboard and mouse back instead of leaving the user with a dead cursor.
    fn on_input_blocked(&mut self) {
        self.clear_portal();
        match self.control.clone() {
            Control::Controlled { peer, .. } => {
                self.sh.platform.release_all();
                self.set_control(Control::Local);
                self.send_to(&peer, Msg::Leave { frac: 0.5 });
                self.notify(
                    "Windows is showing a protected screen (like an admin prompt). \
                     Use this computer's own mouse and keyboard for it.",
                );
            }
            Control::Remote { peer, .. } => {
                let (x, y) = self.center();
                self.sh.platform.end_capture(x, y);
                self.set_control(Control::Local);
                self.send_to(&peer, Msg::Release);
                self.notify("Windows is showing a protected screen here, so control came back.");
            }
            Control::Local => {}
        }
    }

    /// Shows a short message in the UI; it clears itself after a few seconds.
    fn notify(&mut self, msg: &str) {
        self.sh.status.lock().unwrap().notice = Some((msg.to_string(), Instant::now()));
        self.sh.publish();
    }

    /// The other computer let go: control comes back to normal here.
    fn on_release(&mut self, from: &str) {
        let plat = self.sh.platform.clone();
        match self.control.clone() {
            Control::Remote { peer, .. } if peer == from => {
                let (x, y) = self.center();
                plat.end_capture(x, y);
                self.set_control(Control::Local);
                self.cancel_outgoing();
                self.held = None;
            }
            Control::Controlled { peer, .. } if peer == from => {
                plat.release_all();
                self.set_control(Control::Local);
            }
            _ => {}
        }
    }

    /// Notes `what` as the move the undo button undoes.
    fn remember(&mut self, what: Undo, label: String) {
        self.last_move = Some((what, Instant::now()));
        self.sh.status.lock().unwrap().undo = Some((label, Instant::now()));
        self.sh.publish();
    }

    /// Files that arrived here (sent or carried): kept for undo, or binned at once if
    /// the sender already took them back.
    fn received_files(&mut self, id: u64, from: String, paths: Vec<PathBuf>) {
        if self.undone.remove(&id) {
            let plat = self.sh.platform.clone();
            tokio::task::spawn_blocking(move || plat.trash(&paths));
            return;
        }
        self.received.push_back((id, from, paths));
        while self.received.len() > 16 {
            self.received.pop_front();
        }
    }

    fn undo_last(&mut self) {
        let Some((what, at)) = self.last_move.take() else {
            return;
        };
        self.sh.status.lock().unwrap().undo = None;
        self.sh.publish();
        if at.elapsed() >= UNDO_FOR {
            return;
        }
        match what {
            Undo::App { peer, app } => {
                let plat = self.sh.platform.clone();
                let a = app.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = plat.launch_app(&a);
                });
                self.send_to(&peer, Msg::UndoApp { app: app.clone() });
                self.notify(&format!("Brought {} back", app.name));
            }
            Undo::Files { peer, id } => {
                self.send_to(&peer, Msg::UndoFiles { id });
                let who = self.peer_name(&peer);
                self.notify(&format!("Took the files back from {who}"));
            }
            Undo::Mirror { peer, id } => {
                if self.bring_home(id) {
                    self.send_to(&peer, Msg::MirrorEnd { id });
                    self.notify("Brought the window back");
                }
            }
        }
    }

    /// How much bigger things from `pid` look here (screens of other resolutions).
    fn peer_k(&self, pid: &str) -> f32 {
        let Some(sess) = self.sessions.get(pid) else {
            return 1.0;
        };
        let Some(theirs) = sess.screen else {
            return 1.0;
        };
        let mine = self.sh.platform.screen();
        crate::span::scale_for(sess.side, theirs, (mine.width, mine.height))
    }

    /// A window from `from` was just let go over here: where the pointer comes in.
    fn take_drop(&mut self, from: &str) -> Option<(i32, i32)> {
        let (p, at, t) = self.span_drop.take()?;
        (p == from && t.elapsed() < Duration::from_secs(3)).then_some(at)
    }

    /// Mirror `id`'s scale (1 if unknown).
    fn mk(&self, id: u64) -> f32 {
        self.mirror_k.get(&id).copied().unwrap_or(1.0)
    }

    /// Our window `id`, shown on another computer, comes back on this screen (the other
    /// computer is told by the caller, if it needs telling). False if it wasn't shown.
    fn bring_home(&mut self, id: u64) -> bool {
        if self.mirrors_out.remove(&id).is_none() {
            return false;
        }
        if self.portal.as_ref().is_some_and(|p| p.owner && p.id == id) {
            match self.control.clone() {
                // Our own mouse is on the real window: it stays here with it.
                Control::Local => self.clear_portal(),
                // Their mouse: it goes back to them.
                _ => {
                    let at = self.portal.as_ref().map(|p| p.at).unwrap_or((0, 0));
                    self.portal_out_remote(at.0, at.1);
                }
            }
        }
        self.sh.platform.mirror_release(id, None);
        self.sync_windows();
        true
    }

    /// Another computer's window `id`, shown here, stops being shown and goes back to it.
    fn send_back(&mut self, id: u64) -> bool {
        let Some(owner) = self.mirrors_in.remove(&id) else {
            return false;
        };
        self.mirror_k.remove(&id);
        if self.portal.as_ref().is_some_and(|p| !p.owner && p.id == id) {
            let (x, y) = self.sh.platform.cursor_pos();
            match self.control.clone() {
                // Our mouse was driving their window: it is ours again, here.
                Control::Remote { peer, .. } if peer == owner => {
                    self.portal = None;
                    self.sh.platform.mirror_cursor(id, None);
                    self.sh.platform.end_capture(x, y);
                    self.set_control(Control::Local);
                }
                _ => self.clear_portal(),
            }
        }
        self.sh.platform.mirror_close(id);
        self.send_to(&owner, Msg::MirrorClosed { id });
        self.sync_windows();
        true
    }

    /// "Bring windows back": every window shown across goes home, both ways.
    fn bring_all_back(&mut self) {
        let mut n = 0;
        for (id, peer) in self.mirrors_out.clone() {
            if self.bring_home(id) {
                self.send_to(&peer, Msg::MirrorEnd { id });
                n += 1;
            }
        }
        for id in self.mirrors_in.keys().copied().collect::<Vec<_>>() {
            if self.send_back(id) {
                n += 1;
            }
        }
        self.notify(&match n {
            0 => "No windows are across".to_string(),
            1 => "Brought the window back".to_string(),
            n => format!("Brought {n} windows back"),
        });
    }

    /// Keeps the bar's count of windows shown across up to date.
    fn sync_windows(&self) {
        let n = (self.mirrors_out.len() + self.mirrors_in.len()) as u32;
        let changed = {
            let mut st = self.sh.status.lock().unwrap();
            std::mem::replace(&mut st.windows, n) != n
        };
        if changed {
            self.sh.publish();
        }
    }

    fn peer_name(&self, pid: &str) -> String {
        self.sessions
            .get(pid)
            .map(|s| s.peer.name.clone())
            .unwrap_or_else(|| "The other computer".into())
    }

    // ---------------------------------------------------------- second-monitor mode
    //
    // The pointer going onto a mirrored window hands it to the computer that owns the
    // window; leaving the window hands it back. Four ways it happens, by which computer
    // has the physical mouse and which has the window:
    //
    // * the mirror's own mouse goes onto it: the mirror starts sending its mouse to the
    //   owner (`portal_in_here` → the owner becomes controlled, `portal_enter`);
    // * the owner's mouse, which is driving the mirror's computer, goes onto it: the
    //   mirror gives the pointer back (`portal_in_controlled` → the owner's own mouse
    //   drives the real window, `portal_enter`);
    // * leaving it with the owner's own mouse: `portal_out_here`;
    // * leaving it with the mirror's mouse: `portal_out_remote`.

    /// Put the pointer back to normal: no window is being driven through a mirror.
    fn clear_portal(&mut self) {
        let Some(p) = self.portal.take() else {
            return;
        };
        if p.owner {
            self.sh.platform.portal_end();
        } else {
            self.sh.platform.mirror_cursor(p.id, None);
            self.sh.platform.pointer_hidden(false);
        }
    }

    /// The mirror end: this computer's own mouse went onto mirror `id` (the hook has
    /// started swallowing it).
    fn portal_in_here(&mut self, id: u64, rx: i32, ry: i32, from_hook: bool) {
        let target = self
            .mirrors_in
            .get(&id)
            .cloned()
            .filter(|_| self.control == Control::Local)
            .and_then(|p| self.sessions.get(&p).map(|s| (p, s.side)));
        let Some((peer, edge)) = target else {
            if from_hook {
                let (x, y) = self.sh.platform.cursor_pos();
                self.sh.platform.end_capture(x, y);
            }
            return;
        };
        self.set_control(Control::Remote {
            peer: peer.clone(),
            edge,
        });
        self.portal = Some(Portal {
            peer: peer.clone(),
            id,
            owner: false,
            at: (rx, ry),
        });
        self.sh.platform.mirror_cursor(id, Some((rx, ry)));
        let k = self.mk(id);
        self.send_to(
            &peer,
            Msg::PortalEnter {
                id,
                rx: crate::span::sc(rx, 1.0 / k),
                ry: crate::span::sc(ry, 1.0 / k),
            },
        );
    }

    /// The mirror end, driven by the owner's mouse: it went onto the owner's window.
    fn portal_in_controlled(&mut self, from: &str, id: u64, rx: i32, ry: i32) {
        self.set_control(Control::Local);
        self.sh.platform.pointer_hidden(true);
        self.portal = Some(Portal {
            peer: from.to_string(),
            id,
            owner: false,
            at: (rx, ry),
        });
        self.sh.platform.mirror_cursor(id, Some((rx, ry)));
        let k = self.mk(id);
        self.send_to(
            from,
            Msg::PortalEnter {
                id,
                rx: crate::span::sc(rx, 1.0 / k),
                ry: crate::span::sc(ry, 1.0 / k),
            },
        );
    }

    /// The owner: the pointer comes onto our window `id`, shown on `from`.
    fn portal_enter(&mut self, from: &str, id: u64, rx: i32, ry: i32) {
        let plat = self.sh.platform.clone();
        let ours = self.mirrors_out.get(&id).map(String::as_str) == Some(from);
        let portal = Portal {
            peer: from.to_string(),
            id,
            owner: true,
            at: (rx, ry),
        };
        match self.control.clone() {
            // Our own mouse, on their screen, went onto it.
            Control::Remote { peer, .. } if ours && peer == from => {
                if let Some((x, y)) = plat.portal_begin(id, rx, ry) {
                    plat.end_capture(x, y);
                    // The pointer is seen on the mirror, over there: not here as well.
                    plat.pointer_hidden(true);
                    self.set_control(Control::Local);
                    self.portal = Some(portal);
                    return;
                }
            }
            // Their mouse went onto it: it drives this computer from here.
            Control::Local if ours => {
                let side = self.sessions.get(from).map(|s| s.side);
                if let (Some(ret), Some((x, y))) = (side, plat.portal_begin(id, rx, ry)) {
                    plat.set_cursor(x, y);
                    plat.pointer_hidden(true);
                    self.set_control(Control::Controlled {
                        peer: from.to_string(),
                        ret,
                    });
                    self.portal = Some(portal);
                    return;
                }
            }
            _ => {}
        }
        // Not now (or the window is gone): the pointer stays over there.
        self.send_to(from, Msg::PortalLeave { id, rx, ry });
    }

    /// The owner, with its own mouse on the real window: the pointer left it (the hook
    /// has started swallowing): it carries on over the mirror.
    fn portal_out_here(&mut self, rx: i32, ry: i32, from_hook: bool) {
        let target = self
            .portal
            .clone()
            .filter(|p| p.owner && self.control == Control::Local)
            .and_then(|p| self.sessions.get(&p.peer).map(|s| (p, s.side)));
        let Some((p, edge)) = target else {
            if from_hook {
                let (x, y) = self.sh.platform.cursor_pos();
                self.sh.platform.end_capture(x, y);
            }
            return;
        };
        self.portal = None;
        self.sh.platform.portal_end();
        self.set_control(Control::Remote {
            peer: p.peer.clone(),
            edge,
        });
        self.send_to(&p.peer, Msg::PortalLeave { id: p.id, rx, ry });
    }

    /// The owner, driven by the mirror's mouse: the pointer left the real window.
    fn portal_out_remote(&mut self, rx: i32, ry: i32) {
        let Some(p) = self.portal.take().filter(|p| p.owner) else {
            return;
        };
        self.sh.platform.portal_end();
        if matches!(&self.control, Control::Controlled { peer, .. } if *peer == p.peer) {
            self.set_control(Control::Local);
        }
        self.send_to(&p.peer, Msg::PortalLeave { id: p.id, rx, ry });
    }

    /// The mirror end: the pointer left the owner's window at (`rx`, `ry`) (or the owner
    /// said "not now"): it carries on here, on the mirror.
    fn portal_leave(&mut self, from: &str, id: u64, rx: i32, ry: i32) {
        let plat = self.sh.platform.clone();
        if !self
            .portal
            .as_ref()
            .is_some_and(|p| !p.owner && p.peer == from && p.id == id)
        {
            return;
        }
        self.portal = None;
        let k = self.mk(id);
        let (rx, ry) = (crate::span::sc(rx, k), crate::span::sc(ry, k));
        let (x, y) = plat
            .mirror_point(id, rx, ry)
            .unwrap_or_else(|| plat.cursor_pos());
        plat.mirror_cursor(id, None);
        match self.control.clone() {
            // The owner's mouse: it drives this computer again.
            Control::Local => {
                plat.set_cursor(x, y);
                plat.pointer_hidden(false);
                if let Some(ret) = self.sessions.get(from).map(|s| s.side) {
                    self.set_control(Control::Controlled {
                        peer: from.to_string(),
                        ret,
                    });
                }
            }
            // Our own mouse: it is ours again.
            Control::Remote { peer, .. } if peer == from => {
                plat.end_capture(x, y);
                self.set_control(Control::Local);
            }
            _ => plat.pointer_hidden(false),
        }
    }

    /// The computer a bare "jump" goes to: the one used last, else the first by name.
    fn default_target(&self) -> Option<String> {
        self.last_peer
            .clone()
            .filter(|p| self.sessions.contains_key(p))
            .or_else(|| {
                self.sh
                    .status
                    .lock()
                    .unwrap()
                    .connected
                    .first()
                    .map(|c| c.id.clone())
            })
    }

    fn engage(&mut self, target: Option<String>) {
        if self.control != Control::Local {
            return;
        }
        let Some(pid) = target
            .filter(|t| self.sessions.contains_key(t))
            .or_else(|| self.default_target())
        else {
            return;
        };
        let Some(edge) = self.sessions.get(&pid).map(|s| s.side) else {
            return;
        };
        self.sh.platform.begin_capture();
        self.last_peer = Some(pid.clone());
        self.set_control(Control::Remote {
            peer: pid.clone(),
            edge,
        });
        self.send_to(&pid, Msg::Enter { edge, frac: 0.5 });
    }

    fn on_msg(&mut self, from: &str, m: Msg) {
        let plat = self.sh.platform.clone();
        match m {
            Msg::Ping => self.send_to(from, Msg::Pong),
            Msg::License { pass } => {
                let ok = self.peer_pass_ok(from, pass.as_deref());
                if let Some(s) = self.sessions.get_mut(from) {
                    s.peer_licensed = ok;
                    s.license_seen = true;
                }
                self.enforce_license();
            }
            Msg::Pong => {
                if let Some(s) = self.sessions.get_mut(from) {
                    if let Some(at) = s.ping_at.take() {
                        let ms = at.elapsed().as_millis().min(10_000) as u32;
                        let mut d = self.sh.devices.lock().unwrap();
                        if let Some(e) = d.peers.get_mut(from) {
                            // Smoothed, so one slow packet doesn't make the card jump.
                            e.1 = Some(match e.1 {
                                Some(old) => (old * 3 + ms) / 4,
                                None => ms,
                            });
                        }
                    }
                }
            }
            Msg::Bye => {
                self.no_auto
                    .insert(from.to_string(), Instant::now() + Duration::from_secs(10));
                self.drop_session(from, "peer closed the connection")
            }
            Msg::Layout { peer_side } => {
                let mine = peer_side.opposite();
                self.apply_side(from, mine, true);
            }
            Msg::Enter { edge, frac } => {
                if self.control == Control::Local {
                    self.carry = None;
                    self.early_release = None;
                    // Our own files, brought back with the button still down, are in hand again.
                    self.holding_local =
                        self.left_down && self.held.as_ref().is_some_and(|h| h.peer == from);
                    if !self.holding_local {
                        self.held = None;
                    }
                    self.sync_carry();
                    let (x, y) = self
                        .take_drop(from)
                        .unwrap_or_else(|| edge_point(plat.screen(), edge.opposite(), frac));
                    plat.set_cursor(x, y);
                    let me = self.sh.me.id.clone();
                    self.glow(edge.opposite(), frac, &me);
                    self.set_control(Control::Controlled {
                        peer: from.to_string(),
                        ret: edge.opposite(),
                    });
                } else {
                    self.send_to(from, Msg::Release); // "not now"
                }
            }
            Msg::Leave { frac } => {
                if let Control::Remote { peer, edge } = self.control.clone() {
                    if peer == from {
                        let (x, y) = self
                            .take_drop(from)
                            .unwrap_or_else(|| edge_point(plat.screen(), edge, frac));
                        plat.end_capture(x, y);
                        let me = self.sh.me.id.clone();
                        self.glow(edge, frac, &me);
                        self.set_control(Control::Local);
                        self.just_left = Some((from.to_string(), Instant::now()));
                        self.holding_local =
                            self.left_down && self.held.as_ref().is_some_and(|h| h.peer == from);
                        // The files never left: stop sending them over.
                        self.cancel_outgoing();
                        if !self.holding_local {
                            self.held = None;
                        }
                        self.sync_carry();
                    }
                }
            }
            Msg::CarryEnd => {
                if self.held.as_ref().is_some_and(|h| h.peer == from) {
                    self.held = None;
                    self.holding_local = false;
                    self.sync_carry();
                }
            }
            Msg::Release => {
                if self.portal.as_ref().is_some_and(|p| p.peer == from) {
                    self.clear_portal();
                }
                self.on_release(from);
            }
            Msg::Input(ev) => {
                let ret = match &self.control {
                    Control::Controlled { peer, ret } if peer == from => *ret,
                    _ => return,
                };
                // The pointer is on one of our windows that is shown over there: it stays
                // on it until it leaves it, then carries on over the mirror.
                if self
                    .portal
                    .as_ref()
                    .is_some_and(|p| p.owner && p.peer == from)
                {
                    self.track_button(&ev);
                    plat.inject(&ev);
                    if matches!(ev, InputEvent::MouseMove { .. }) && !self.left_down {
                        if let Some((rx, ry)) = plat.portal_outside() {
                            self.portal_out_remote(rx, ry);
                        }
                    }
                    return;
                }
                // One of our windows hangs across the way back: the mouse moves it.
                if self.span_driving {
                    match ev {
                        InputEvent::MouseMove { dx, dy } => plat.span_move(dx, dy),
                        InputEvent::Button {
                            button: Button::Left,
                            down: false,
                        } => {
                            self.left_down = false;
                            plat.span_release();
                        }
                        InputEvent::Button {
                            button: Button::Right,
                            down: true,
                        } => plat.span_cancel(),
                        // Esc (virtual key 0x1B).
                        InputEvent::Key {
                            vk: 0x1B,
                            down: true,
                            ..
                        } => plat.span_cancel(),
                        _ => {}
                    }
                    return;
                }
                self.track_button(&ev);
                plat.inject(&ev);
                if let InputEvent::Button {
                    button: Button::Left,
                    down: false,
                } = ev
                {
                    // Let go during a preview: the other computer stops showing it.
                    plat.span_release();
                }
                if let InputEvent::Button {
                    button: Button::Left,
                    down: false,
                } = ev
                {
                    if self.holding_local {
                        self.drop_held_locally();
                    } else {
                        self.on_carry_release(from);
                    }
                }
                if let InputEvent::MouseMove { dx, dy } = ev {
                    // Onto a window of theirs that is shown here: the pointer goes to it.
                    if !self.left_down && !self.mirrors_in.is_empty() {
                        if let Some((id, rx, ry)) = plat.mirror_at_cursor() {
                            if self.mirrors_in.get(&id).map(String::as_str) == Some(from) {
                                self.portal_in_controlled(from, id, rx, ry);
                                return;
                            }
                        }
                    }
                    let s = plat.screen();
                    let (x, y) = plat.cursor_pos();
                    let (fx, fy) = (
                        (x - s.x) as f32 / (s.width - 1).max(1) as f32,
                        (y - s.y) as f32 / (s.height - 1).max(1) as f32,
                    );
                    let leaving = match ret {
                        Edge::Left => (x <= s.x && dx < 0).then_some(fy),
                        Edge::Right => (x >= s.x + s.width - 1 && dx > 0).then_some(fy),
                        Edge::Top => (y <= s.y && dy < 0).then_some(fx),
                        Edge::Bottom => (y >= s.y + s.height - 1 && dy > 0).then_some(fx),
                        // A corner: back through it only by pushing into that very corner.
                        Edge::TopLeft => {
                            (x <= s.x && y <= s.y && (dx < 0 || dy < 0)).then_some(0.5)
                        }
                        Edge::TopRight => {
                            (x >= s.x + s.width - 1 && y <= s.y && (dx > 0 || dy < 0))
                                .then_some(0.5)
                        }
                        Edge::BottomLeft => {
                            (x <= s.x && y >= s.y + s.height - 1 && (dx < 0 || dy > 0))
                                .then_some(0.5)
                        }
                        Edge::BottomRight => (x >= s.x + s.width - 1
                            && y >= s.y + s.height - 1
                            && (dx > 0 || dy > 0))
                            .then_some(0.5),
                    };
                    let moving_apps = self.left_down && self.sh.persisted.lock().unwrap().spans();
                    // A window being dragged here towards the way back: the other
                    // computer shows it as soon as part of it is past the edge...
                    if moving_apps && !ret.is_corner() {
                        if leaving.is_none() {
                            plat.span_watch(ret);
                        } else if plat.span_take(ret) {
                            // ...and when the pointer gets there, it hangs across.
                            self.span_driving = true;
                            return;
                        }
                    }
                    // A program's window (or tab) held against the way back where it
                    // can't hang across: offer to move the program over there instead
                    // of leaving (the cursor stays here so the person can answer).
                    if leaving.is_some() && moving_apps {
                        match plat.app_hold() {
                            AppHold::Reading => return,
                            AppHold::Ready(app) => {
                                log::info!("app move: {} dragged back towards {from}", app.exe);
                                plat.cancel_drag();
                                self.offer_app(from.to_string(), app);
                                return;
                            }
                            AppHold::Nothing => {}
                        }
                    }
                    if let Some(frac) = leaving {
                        // Leaving with files in hand (picked up on this computer): take
                        // them to the other one and stop the drag here.
                        let files = if self.left_down
                            && !self.holding_local
                            && self.sh.persisted.lock().unwrap().drag_files
                        {
                            plat.take_drag_files().filter(|p| !p.is_empty())
                        } else {
                            None
                        };
                        let mine = self.holding_local;
                        plat.release_all();
                        if files.is_some() {
                            plat.cancel_drag();
                        }
                        self.set_control(Control::Local);
                        self.send_to(from, Msg::Leave { frac });
                        self.glow(ret, frac, from);
                        if mine {
                            // Our own files go back over with the cursor.
                            self.holding_local = false;
                            self.sync_carry();
                        }
                        if let Some(paths) = files {
                            let offer =
                                self.build_offer(paths.clone(), OfferKind::Carry, from.to_string());
                            self.held = Some(Held {
                                peer: from.to_string(),
                                offer,
                                view: carry_view(&paths),
                                paths,
                            });
                            self.notify("Carrying files — let go on the other screen to drop");
                        }
                    }
                }
            }
            Msg::ClipText(t) => {
                let on = self.sh.persisted.lock().unwrap().clipboard_sync;
                if on && t.len() <= MAX_CLIP_TEXT {
                    let c = ClipContent::Text(t.clone());
                    let s = sig(&c);
                    // Seen already (it came round again): neither apply nor pass on.
                    if self.last_clip_sig == Some(s) {
                        return;
                    }
                    self.last_clip_sig = Some(s);

                    let _ = self.clip.send(ClipJob::Write(c));
                    // A computer in the middle passes it on to the others.
                    self.send_all_but(Some(from), &Msg::ClipText(t));
                }
            }
            Msg::ClipImage(png) => {
                log::info!("clipboard: received a picture ({} KB)", png.len() / 1024);
                let on = self.sh.persisted.lock().unwrap().clipboard_sync;
                if on && !png.is_empty() && png.len() <= MAX_CLIP_IMAGE {
                    let c = ClipContent::Image(png.clone());
                    let s = sig(&c);
                    if self.last_clip_sig == Some(s) {
                        return;
                    }
                    self.last_clip_sig = Some(s);
                    self.clip_written_at = Some(Instant::now());
                    let _ = self.clip.send(ClipJob::Write(c));
                    self.send_all_but(Some(from), &Msg::ClipImage(png));
                }
            }
            Msg::MoveApp { id, app } => {
                log::info!("app move: {} asked to open {}", from, app.exe);
                let refuse = |me: &Self, note: &str| {
                    me.send_to(
                        from,
                        Msg::MoveAppAnswer {
                            id,
                            ok: false,
                            note: note.to_string(),
                        },
                    );
                };
                if !self.sh.persisted.lock().unwrap().move_apps {
                    refuse(self, "Moving apps is turned off on that computer");
                    return;
                }
                if !app.is_sane() {
                    refuse(self, "That app can't be moved");
                    return;
                }
                let plat = self.sh.platform.clone();
                let ev_tx = self.ev_tx.clone();
                let from = from.to_string();
                // Finding the program can take a moment: never on the engine's thread.
                tokio::task::spawn_blocking(move || {
                    let (ok, note) = match plat.launch_app(&app) {
                        Ok(()) => (true, String::new()),
                        Err(e) => (false, e),
                    };
                    plat.span_hide(if ok { 900 } else { 0 });
                    let _ = ev_tx.send(Ev::AppMoved {
                        from,
                        id,
                        name: app.name,
                        ok,
                        note,
                    });
                });
            }
            Msg::MoveAppAnswer { id, ok, note } => {
                let Some((peer, app)) = self.pending_app.get(&id).cloned() else {
                    return;
                };
                if peer != from {
                    return;
                }
                self.pending_app.remove(&id);
                let who = self
                    .sessions
                    .get(from)
                    .map(|s| s.peer.name.clone())
                    .unwrap_or_else(|| "the other computer".into());
                if !ok {
                    self.sh.platform.span_restore();
                    let m = if note.is_empty() {
                        format!("{} didn't open on {who}", app.name)
                    } else {
                        note
                    };
                    self.notify(&m);
                } else if app.url.is_some() {
                    self.sh.platform.span_restore();
                    self.notify(&format!("Opened the page on {who}"));
                } else {
                    let plat = self.sh.platform.clone();
                    let a = app.clone();
                    tokio::task::spawn_blocking(move || plat.close_app(&a));
                    self.notify(&format!("Moved {} to {who}", app.name));
                    self.remember(
                        Undo::App {
                            peer: from.to_string(),
                            app: app.clone(),
                        },
                        format!("{} to {who}", app.name),
                    );
                }
            }
            Msg::FileOffer { id, kind, items } => self.on_file_offer(from, id, kind, items),
            Msg::Status(mut info) => {
                info.accessories.truncate(12);
                for a in &mut info.accessories {
                    a.name.truncate(48);
                }
                info.mac = info.mac.filter(|m| is_mac(m));
                if let Some(mac) = info.mac.clone() {
                    let mut per = self.sh.persisted.lock().unwrap();
                    let mut dirty = false;
                    if let Some(p) = per.peers.iter_mut().find(|p| p.id == from) {
                        if p.mac.as_deref() != Some(mac.as_str()) {
                            p.mac = Some(mac);
                            dirty = true;
                        }
                    }
                    drop(per);
                    if dirty {
                        self.sh.save();
                    }
                }
                {
                    let mut d = self.sh.devices.lock().unwrap();
                    let ping = d.peers.get(from).and_then(|e| e.1);
                    d.peers.insert(from.to_string(), (info, ping));
                }
                self.sh.publish();
            }
            Msg::Power(a) => {
                log::info!("power: {from} asked to {a:?} this computer");
                let plat = self.sh.platform.clone();
                tokio::task::spawn_blocking(move || plat.power(a));
            }
            Msg::SendApps => {
                if !self.sh.persisted.lock().unwrap().move_apps {
                    self.send_to(
                        from,
                        Msg::MoveAppAnswer {
                            id: 0,
                            ok: false,
                            note: "Moving apps is turned off on that computer".into(),
                        },
                    );
                    return;
                }
                let plat = self.sh.platform.clone();
                let ev_tx = self.ev_tx.clone();
                let to = from.to_string();
                tokio::task::spawn_blocking(move || {
                    let apps = plat.open_apps();
                    log::info!("app move: sending {} open programs over", apps.len());
                    for app in apps {
                        let _ = ev_tx.send(Ev::AppPicked {
                            to: to.clone(),
                            res: Ok(app),
                        });
                        std::thread::sleep(Duration::from_millis(400));
                    }
                });
            }
            Msg::MirrorStart { id, w, h } => {
                let entry = self.sessions.get(from).map(|s| s.side);
                // Shown at the size it has on its own screen, relative to that screen.
                let k = self.peer_k(from);
                let (sw, sh) = (
                    crate::span::sc(w as i32, k).max(1) as u32,
                    crate::span::sc(h as i32, k).max(1) as u32,
                );
                let ok = w > 0
                    && h > 0
                    && w <= 16384
                    && h <= 16384
                    && plat.mirror_supported()
                    && entry.is_some_and(|e| plat.mirror_create(id, sw, sh, e));
                if ok {
                    self.mirrors_in.insert(id, from.to_string());
                    self.mirror_k.insert(id, k);
                    self.sync_windows();
                } else {
                    plat.span_hide(0);
                    self.send_to(from, Msg::MirrorEnd { id });
                }
            }
            Msg::MirrorFrame { id, w, h, jpeg } => {
                if self.mirrors_in.get(&id).map(String::as_str) == Some(from) {
                    let k = self.mk(id);
                    let (sw, sh) = (
                        crate::span::sc(w as i32, k).max(1) as u32,
                        crate::span::sc(h as i32, k).max(1) as u32,
                    );
                    plat.mirror_frame(id, sw, sh, jpeg);
                }
            }
            Msg::MirrorEnd { id } => {
                if self.mirrors_in.get(&id).map(String::as_str) == Some(from) {
                    self.mirrors_in.remove(&id);
                    self.mirror_k.remove(&id);
                    if self.portal.as_ref().is_some_and(|p| !p.owner && p.id == id) {
                        self.clear_portal();
                    }
                    plat.mirror_close(id);
                } else if self.mirrors_out.get(&id).map(String::as_str) == Some(from) {
                    // They can't show it: it comes back here.
                    self.mirrors_out.remove(&id);
                    plat.mirror_release(id, None);
                    let who = self.peer_name(from);
                    self.notify(&format!(
                        "{who} can't show windows yet: update CubeCursor there"
                    ));
                }
            }
            Msg::PortalEnter { id, rx, ry } => self.portal_enter(from, id, rx, ry),
            Msg::PortalLeave { id, rx, ry } => self.portal_leave(from, id, rx, ry),
            Msg::PortalCursor { id, rx, ry } => {
                let k = self.mk(id);
                if let Some(p) = self
                    .portal
                    .as_mut()
                    .filter(|p| !p.owner && p.peer == from && p.id == id)
                {
                    let (rx, ry) = (crate::span::sc(rx, k), crate::span::sc(ry, k));
                    p.at = (rx, ry);
                    plat.mirror_cursor(id, Some((rx, ry)));
                }
            }
            Msg::MirrorMoved { id, dx, dy, done } => {
                if self.mirrors_in.get(&id).map(String::as_str) != Some(from) {
                    return;
                }
                let k = self.mk(id);
                let (dx, dy) = (crate::span::sc(dx, k), crate::span::sc(dy, k));
                if let Some(along) = plat.mirror_move(id, dx, dy, done) {
                    // Dragged back over the edge: the window goes home.
                    self.mirrors_in.remove(&id);
                    if self.portal.as_ref().is_some_and(|p| !p.owner && p.id == id) {
                        // The pointer is on the real window, which is over there now.
                        plat.mirror_cursor(id, None);
                        if self.control == Control::Local {
                            plat.pointer_hidden(false);
                        }
                        self.portal = None;
                    }
                    self.send_to(from, Msg::MirrorHome { id, along });
                }
            }
            Msg::MirrorHome { id, along } => {
                if self.mirrors_out.get(&id).map(String::as_str) == Some(from) {
                    self.mirrors_out.remove(&id);
                    if self.portal.as_ref().is_some_and(|p| p.owner && p.id == id) {
                        self.portal = None;
                    }
                    plat.mirror_release(id, Some(along));
                }
            }
            Msg::UndoApp { app } => {
                if !app.is_sane() {
                    return;
                }
                let who = self.peer_name(from);
                let plat = self.sh.platform.clone();
                let a = app.clone();
                tokio::task::spawn_blocking(move || plat.close_app(&a));
                self.notify(&format!("{} went back to {who}", app.name));
            }
            Msg::UndoFiles { id } => {
                let found = self
                    .received
                    .iter()
                    .position(|(i, f, _)| *i == id && f == from);
                match found.and_then(|i| self.received.remove(i)) {
                    Some((_, _, paths)) => {
                        let plat = self.sh.platform.clone();
                        let n = paths.len();
                        tokio::task::spawn_blocking(move || plat.trash(&paths));
                        let who = self.peer_name(from);
                        self.notify(&format!(
                            "{who} took back {n} item{}: in the bin",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    None => {
                        // Not here yet: they go to the bin as soon as they arrive.
                        self.undone.insert(id);
                        if self.undone.len() > 32 {
                            self.undone.clear();
                        }
                    }
                }
            }
            Msg::Screen { w, h } => {
                if (1..=65536).contains(&w) && (1..=65536).contains(&h) {
                    if let Some(sess) = self.sessions.get_mut(from) {
                        sess.screen = Some((w, h));
                    }
                }
            }
            Msg::MirrorClosed { id } => {
                if self.mirrors_out.get(&id).map(String::as_str) == Some(from) {
                    log::info!("mirror: window {id} was closed over there: it comes back");
                    self.bring_home(id);
                }
            }
            Msg::SpanBegin { w, h } => {
                if w == 0 || h == 0 || w > 16384 || h > 16384 {
                    return;
                }
                self.span_in = Some((from.to_string(), w, h));
            }
            Msg::SpanPos {
                crossed,
                along,
                gx,
                gy,
            } => {
                let Some((p, w, h)) = self.span_in.clone() else {
                    return;
                };
                let Some(entry) = self.sessions.get(from).map(|s| s.side) else {
                    return;
                };
                if p != from || entry.is_corner() || !along.is_finite() {
                    return;
                }
                let v = crate::span::scale_view(
                    SpanView {
                        entry,
                        crossed,
                        along: along.clamp(-1.0, 2.0),
                        w,
                        h,
                        gx,
                        gy,
                    },
                    self.peer_k(from),
                );
                self.span_view = Some(v);
                self.sh.platform.span_show(v);
            }
            Msg::SpanFrame(jpeg) => {
                if self.span_in.as_ref().is_some_and(|(p, ..)| p == from) {
                    self.sh.platform.span_frame(jpeg);
                }
            }
            Msg::SpanEnd { moved } => {
                if self.span_in.as_ref().is_some_and(|(p, ..)| p == from) {
                    self.span_in = None;
                    // Let go over here: the pointer comes in where it was holding it.
                    if let Some(v) = self.span_view.take().filter(|_| moved) {
                        let at = crate::span::pointer_at(&v, self.sh.platform.screen());
                        self.span_drop = Some((from.to_string(), at, Instant::now()));
                    }
                    // Moving: keep the picture up while the real program opens.
                    self.sh.platform.span_hide(if moved { 6000 } else { 0 });
                }
            }
        }
    }

    /// Ask the person here whether to move `app` to the computer `to`, then send it.
    fn offer_app(&mut self, to: String, app: AppRef) {
        let Some(who) = self.sessions.get(&to).map(|s| s.peer.name.clone()) else {
            return;
        };
        let plat = self.sh.platform.clone();
        let ev_tx = self.ev_tx.clone();
        // The question waits for a person: never on the engine's thread.
        tokio::task::spawn_blocking(move || {
            let res = if !app.is_sane() {
                Err("That window can't be moved".to_string())
            } else if plat.confirm_move_app(&who, &app) {
                Ok(app)
            } else {
                Err(String::new())
            };
            let _ = ev_tx.send(Ev::AppPicked { to, res });
        });
    }

    fn announce_side(&self, pid: &str) {
        if let Some(s) = self.sessions.get(pid) {
            let _ = s.tx.send(Msg::Layout { peer_side: s.side });
        }
    }

    fn set_side_raw(&mut self, pid: &str, side: Edge) {
        {
            let mut per = self.sh.persisted.lock().unwrap();
            if let Some(p) = per.peers.iter_mut().find(|p| p.id == pid) {
                p.side = side;
            }
        }
        if let Some(s) = self.sessions.get_mut(pid) {
            s.side = side;
        }
        self.sh.save();
        self.refresh_connected();
    }

    /// Puts computer `pid` on `side` of my screen. `from_peer`: the other computer chose
    /// this layout, so if the side is taken it is the newcomer that moves; when the user
    /// chose it, the two computers swap sides.
    fn apply_side(&mut self, pid: &str, side: Edge, from_peer: bool) {
        let old = {
            let per = self.sh.persisted.lock().unwrap();
            match per.peers.iter().find(|p| p.id == pid) {
                Some(p) => p.side,
                None => return,
            }
        };
        let holder = self
            .sh
            .persisted
            .lock()
            .unwrap()
            .peers
            .iter()
            .find(|p| p.id != pid && p.side == side)
            .map(|p| p.id.clone());
        let mut side = side;
        let mut tell_peer = !from_peer;
        if let Some(h) = holder {
            let free = if from_peer {
                self.first_free_side(pid)
            } else {
                None
            };
            match free {
                Some(f) => {
                    side = f;
                    tell_peer = true;
                }
                None => {
                    self.set_side_raw(&h, old);
                    self.announce_side(&h);
                }
            }
        }
        self.set_side_raw(pid, side);
        if tell_peer {
            self.announce_side(pid);
        }
    }

    fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Connect { addr, port, code } => {
                if self.sessions.len() >= MAX_PEERS {
                    self.sh.set_error(Some(
                        "Four computers are already connected — disconnect one first".into(),
                    ));
                } else {
                    self.sh.set_error(None);
                    self.start_dial(SocketAddr::new(addr, port), ClientAuth::Pair { code });
                }
            }
            Cmd::ConnectKnown { peer_id } => {
                self.no_auto.remove(&peer_id);
                let per = self.sh.persisted.lock().unwrap().clone();
                let Some(p) = per.peers.iter().find(|p| p.id == peer_id) else {
                    return;
                };
                let disc = self.sh.discovered.lock().unwrap().get(&peer_id).cloned();
                let target = disc.map(|(d, _)| (d.addr, d.port)).or_else(|| {
                    p.last_addr
                        .as_ref()
                        .and_then(|a| a.parse::<SocketAddr>().ok())
                        .map(|a| (a.ip(), a.port()))
                });
                if let (Some((ip, port)), Some(key)) = (target, unhex32(&p.link_key)) {
                    if !self.sessions.contains_key(&peer_id) && self.sessions.len() < MAX_PEERS {
                        self.sh.set_error(None);
                        self.start_dial(
                            SocketAddr::new(ip, port),
                            ClientAuth::Known { link_key: key },
                        );
                    }
                }
            }
            Cmd::Disconnect(which) => {
                let ids: Vec<String> = match which {
                    Some(id) => vec![id],
                    None => self.sessions.keys().cloned().collect(),
                };
                for id in ids {
                    if !self.sessions.contains_key(&id) {
                        continue;
                    }
                    self.no_auto
                        .insert(id.clone(), Instant::now() + Duration::from_secs(30));
                    self.send_to(&id, Msg::Bye);
                    self.drop_session(&id, "");
                }
            }
            Cmd::SetSide { peer_id, side } => self.apply_side(&peer_id, side, false),
            Cmd::Forget(id) => {
                if self.sessions.contains_key(&id) {
                    self.send_to(&id, Msg::Bye);
                    self.drop_session(&id, "");
                }
                if self.last_peer.as_deref() == Some(id.as_str()) {
                    self.last_peer = None;
                }
                self.sh
                    .persisted
                    .lock()
                    .unwrap()
                    .peers
                    .retain(|p| p.id != id);
                self.sh.save();
                self.sh.publish();
            }
            Cmd::SetClipboardSync(on) => {
                self.sh.persisted.lock().unwrap().clipboard_sync = on;
                self.sh.save();
                self.sh.publish();
            }
            Cmd::SetGameMode(on) => {
                self.sh.persisted.lock().unwrap().game_mode = on;
                self.sh.platform.set_game_mode(on);
                self.sh.save();
                self.sh.publish();
            }
            Cmd::SetLicensePass(pass) => {
                self.my_pass = pass;
                self.license_changed();
            }
            Cmd::Power { peer_id, action } => self.send_to(&peer_id, Msg::Power(action)),
            Cmd::TakeApps(peer_id) => {
                if self.sessions.contains_key(&peer_id) {
                    self.notify("Bringing your programs over…");
                    self.send_to(&peer_id, Msg::SendApps);
                }
            }
            Cmd::Wake(peer_id) => {
                let (mac, name, addr) = {
                    let per = self.sh.persisted.lock().unwrap();
                    match per.peers.iter().find(|p| p.id == peer_id) {
                        Some(p) => (p.mac.clone(), p.name.clone(), p.last_addr.clone()),
                        None => return,
                    }
                };
                match mac.as_deref().and_then(parse_mac) {
                    Some(m) => {
                        let ip = addr
                            .and_then(|a| a.parse::<std::net::SocketAddr>().ok())
                            .map(|a| a.ip());
                        tokio::task::spawn_blocking(move || send_wake(m, ip));
                        self.notify(&format!("Waking {name}…"));
                    }
                    None => self.notify(&format!("{name} hasn't told us how to wake it yet")),
                }
            }
            Cmd::SetMoveApps(on) => {
                let spans = {
                    let mut p = self.sh.persisted.lock().unwrap();
                    p.move_apps = on;
                    p.spans()
                };
                self.sh.platform.set_move_apps(spans);
                self.sh.save();
                self.sh.publish();
            }
            Cmd::Undo => self.undo_last(),
            Cmd::BringBack => self.bring_all_back(),
            Cmd::SetMonitorMode(on) => {
                let spans = {
                    let mut p = self.sh.persisted.lock().unwrap();
                    p.monitor_mode = on;
                    p.spans()
                };
                self.sh.platform.set_move_apps(spans);
                self.sh.save();
                self.sh.publish();
            }
            Cmd::SetDragFiles(on) => {
                self.sh.persisted.lock().unwrap().drag_files = on;
                self.sh.platform.set_drag_files(on);
                self.sh.save();
                self.sh.publish();
            }
            Cmd::SetSwitchDelay(ms) => {
                let ms = ms.min(3000);
                self.sh.persisted.lock().unwrap().switch_delay_ms = ms;
                self.sh.platform.set_switch_delay(ms);
                self.sh.save();
                self.sh.publish();
            }
            Cmd::SetName(n) => {
                let n: String = n.chars().filter(|c| !c.is_control()).take(48).collect();
                if !n.trim().is_empty() {
                    self.sh.persisted.lock().unwrap().device_name = n;
                    self.sh.save();
                    self.sh.publish();
                }
            }
            Cmd::NewCode => {
                let mut p = self.sh.pairing.lock().unwrap();
                p.code = new_code();
                p.attempts_left = 3;
                drop(p);
                self.sh.publish();
            }
            Cmd::Engage(target) => self.engage(target),
            Cmd::SendFiles { paths, target } => {
                let targets = self.file_targets(target);
                if targets.is_empty() {
                    self.sh
                        .set_error(Some("Connect to a computer first".into()));
                }
                for t in targets {
                    self.build_offer(paths.clone(), OfferKind::Send, t);
                }
            }
        }
    }

    /// Who a "send files" goes to: the named computer; otherwise the one the mouse is on
    /// (or that is driving us); otherwise every connected computer.
    fn file_targets(&self, target: Option<String>) -> Vec<String> {
        if let Some(t) = target {
            return if self.sessions.contains_key(&t) {
                vec![t]
            } else {
                vec![]
            };
        }
        if let Control::Remote { peer, .. } | Control::Controlled { peer, .. } = &self.control {
            if self.sessions.contains_key(peer) {
                return vec![peer.clone()];
            }
        }
        let mut all: Vec<String> = self.sessions.keys().cloned().collect();
        all.sort();
        all
    }

    // ---- clipboard & files

    fn on_clipboard_changed(&mut self) {
        let on = self.sh.persisted.lock().unwrap().clipboard_sync;
        if !on || self.sessions.is_empty() {
            return;
        }
        let _ = self.clip.send(ClipJob::Read);
    }

    /// The clipboard worker finished reading: send it on unless it is our own echo.
    fn on_clip_read(&mut self, c: ClipContent) {
        let on = self.sh.persisted.lock().unwrap().clipboard_sync;
        if !on || self.sessions.is_empty() {
            return;
        }
        let s = sig(&c);
        if self.last_clip_sig == Some(s) {
            return; // our own echo, or unchanged
        }
        self.last_clip_sig = Some(s);
        // A picture we just wrote coming back a moment later, re-encoded by Windows:
        // remember it, but don't send it back.
        if matches!(c, ClipContent::Image(_))
            && self
                .clip_written_at
                .is_some_and(|w| w.elapsed() < Duration::from_millis(700))
        {
            return;
        }
        match c {
            ClipContent::Text(t) => {
                if !t.is_empty() && t.len() <= MAX_CLIP_TEXT {
                    self.send_all_but(None, &Msg::ClipText(t));
                }
            }
            ClipContent::Files(paths) => {
                let mut ids: Vec<String> = self.sessions.keys().cloned().collect();
                ids.sort();
                for id in ids {
                    self.build_offer(paths.clone(), OfferKind::Clipboard, id);
                }
            }
            ClipContent::Image(png) => {
                if !png.is_empty() && png.len() <= MAX_CLIP_IMAGE {
                    log::info!("clipboard: sending a picture ({} KB)", png.len() / 1024);
                    self.send_all_but(None, &Msg::ClipImage(png));
                } else {
                    log::info!(
                        "clipboard: picture too big to sync ({} KB)",
                        png.len() / 1024
                    );
                    self.notify("That picture is too big to copy across");
                }
            }
        }
    }

    fn build_offer(&mut self, paths: Vec<PathBuf>, kind: OfferKind, target: String) -> u64 {
        let id: u64 = rand::thread_rng().gen();
        let ev_tx = self.ev_tx.clone();
        tokio::task::spawn_blocking(move || {
            let res = files::build_manifest(&paths);
            let _ = ev_tx.send(Ev::OfferBuilt {
                id,
                kind,
                target,
                res,
            });
        });
        id
    }

    fn on_offer_built(
        &mut self,
        id: u64,
        kind: OfferKind,
        target: String,
        res: std::io::Result<Offer>,
    ) {
        if self.called_off.remove(&id) {
            return;
        }
        let offer = match res {
            Ok(o) if !o.items.is_empty() => Arc::new(o),
            Ok(_) => return,
            Err(e) => return self.sh.set_error(Some(format!("Cannot send files: {e}"))),
        };
        let Some(peer_name) = self.sessions.get(&target).map(|s| s.peer.name.clone()) else {
            return;
        };
        {
            let mut q = self.sh.offers.lock().unwrap();
            q.push_back((id, offer.clone()));
            while q.len() > 16 {
                q.pop_front();
            }
        }
        let first = offer.items[0]
            .path
            .split('/')
            .next()
            .unwrap_or("files")
            .to_string();
        let tops = offer.items.iter().filter(|i| !i.path.contains('/')).count();
        self.sh.upsert_transfer(TransferView {
            id,
            name: if tops > 1 {
                format!("{first} and {} more", tops - 1)
            } else {
                first
            },
            direction: "out".into(),
            bytes: 0,
            total: files::total_size(&offer.items),
            state: "active".into(),
            message: String::new(),
            path: None,
            peer: peer_name,
        });
        self.send_to(
            &target,
            Msg::FileOffer {
                id,
                kind,
                items: offer.items.clone(),
            },
        );
        if kind != OfferKind::Clipboard {
            let n = tops.max(1);
            let label = format!(
                "{n} item{} to {}",
                if n == 1 { "" } else { "s" },
                self.peer_name(&target)
            );
            self.remember(
                Undo::Files {
                    peer: target.clone(),
                    id,
                },
                label,
            );
        }
    }

    fn on_file_offer(&mut self, from: &str, id: u64, kind: OfferKind, items: Vec<FileItem>) {
        let Some(sess) = self.sessions.get(from) else {
            return;
        };
        let (peer_name, peer_id) = (sess.peer.name.clone(), sess.peer.id.clone());
        let addr = SocketAddr::new(sess.addr, sess.peer.listen_port);
        let per = self.sh.persisted.lock().unwrap().clone();
        let total = files::total_size(&items);
        let tops = items.iter().filter(|i| !i.path.contains('/')).count();
        let first = items
            .first()
            .map(|i| i.path.split('/').next().unwrap_or("files").to_string())
            .unwrap_or_default();
        let name = if tops > 1 {
            format!("{first} and {} more", tops - 1)
        } else {
            first
        };
        let view = |state: &str, message: &str| TransferView {
            id,
            name: name.clone(),
            direction: "in".into(),
            bytes: 0,
            total,
            state: state.into(),
            message: message.into(),
            path: None,
            peer: peer_name.clone(),
        };
        if items.is_empty()
            || items.len() > files::MAX_ITEMS
            || items.iter().any(|i| files::sanitize_rel(&i.path).is_none())
        {
            return self.sh.upsert_transfer(view(
                "error",
                "The other computer sent an invalid file list",
            ));
        }
        if kind == OfferKind::Clipboard
            && (!per.clipboard_sync || total > per.auto_file_limit_mb * 1024 * 1024)
        {
            if per.clipboard_sync {
                self.sh.upsert_transfer(view(
                    "error",
                    "Copied files are too large to sync automatically — use Send files",
                ));
            }
            return;
        }
        if kind == OfferKind::Carry {
            // Only while that computer's mouse is the one on our screen.
            let ours = match &self.control {
                Control::Controlled { peer, .. } => peer == from,
                // The other computer's cursor just came back to us with files in hand.
                Control::Local => self
                    .just_left
                    .as_ref()
                    .is_some_and(|(p, at)| p == from && at.elapsed() < Duration::from_secs(3)),
                Control::Remote { .. } => false,
            };
            if !ours || !per.drag_files {
                return;
            }
            self.carry = Some(Carry {
                id,
                peer: from.to_string(),
                view: CarryView {
                    name: name.clone(),
                    count: tops,
                    folder: items
                        .first()
                        .is_some_and(|i| i.is_dir && !i.path.contains('/')),
                },
                staged: None,
                fetch: None,
                dir: None,
            });
            self.sync_carry();
            self.notify("Files incoming — let go of the mouse where you want them");
        }
        let Some(key) = unhex32(
            &per.peers
                .iter()
                .find(|p| p.id == peer_id)
                .map(|p| p.link_key.clone())
                .unwrap_or_default(),
        ) else {
            return;
        };
        let dest = match kind {
            OfferKind::Clipboard => self.sh.cfg.temp_dir.join(format!("clip-{id:016x}")),
            OfferKind::Send => self.sh.cfg.download_dir.clone(),
            OfferKind::Carry => self.sh.cfg.temp_dir.join(format!("carry-{id:016x}")),
        };
        self.sh.upsert_transfer(view("active", ""));
        if kind == OfferKind::Carry {
            // The button already went up before the offer reached us: drop it right away.
            if let Some(dir) = self.early_release.take() {
                self.drops.insert(id, dir);
                self.carry = None;
                self.sync_carry();
            }
        }
        let (sh, ev_tx) = (self.sh.clone(), self.ev_tx.clone());
        let fetch_dest = dest.clone();
        let from_id = from.to_string();
        if kind == OfferKind::Carry {
            self.carry_from.insert(id, from_id.clone());
        }
        let task = tokio::spawn(async move {
            let res = fetch_files(&sh, addr, key, id, &items, &dest).await;
            match res {
                Ok(paths) => {
                    if kind == OfferKind::Carry {
                        // Not finished yet: it is done once dropped where the user chose.
                        let _ = ev_tx.send(Ev::CarryFetched { id, paths });
                        return;
                    }
                    let shown = if kind == OfferKind::Send {
                        let _ = ev_tx.send(Ev::Received {
                            id,
                            from: from_id.clone(),
                            paths: paths.clone(),
                        });
                        Some(dest.to_string_lossy().into_owned())
                    } else {
                        None
                    };
                    sh.transfer_finish(id, "in", Ok(shown));
                    if kind == OfferKind::Clipboard {
                        let _ = ev_tx.send(Ev::ClipFiles(paths));
                    }
                }
                Err(e) => sh.transfer_finish(id, "in", Err(e.to_string())),
            }
        });
        if kind == OfferKind::Carry {
            if let Some(c) = self.carry.as_mut().filter(|c| c.id == id) {
                c.fetch = Some(task.abort_handle());
                c.dir = Some(fetch_dest);
            }
        }
    }

    /// The left button went up while `from`'s mouse is on our screen: if it was carrying
    /// files, they land in the folder under the cursor.
    fn on_carry_release(&mut self, from: &str) {
        let (x, y) = self.sh.platform.cursor_pos();
        let dir = || {
            self.sh
                .platform
                .drop_dir_at(x, y)
                .unwrap_or_else(|| self.sh.cfg.download_dir.clone())
        };
        let taken = self.carry.take();
        self.sync_carry();
        if taken.as_ref().is_some_and(|c| c.peer == from) {
            self.send_to(from, Msg::CarryEnd);
        }
        match taken {
            Some(c) if c.peer == from => {
                let dir = dir();
                match c.staged {
                    Some(paths) => self.finish_drop(c.id, paths, dir),
                    None => {
                        self.drops.insert(c.id, dir);
                    }
                }
            }
            other => {
                self.carry = other;
                self.sync_carry();
                // Perhaps the offer is still on its way.
                if self.carry.is_none() {
                    self.early_release = Some(dir());
                }
            }
        }
    }

    fn finish_drop(&self, id: u64, paths: Vec<PathBuf>, dir: PathBuf) {
        let ev_tx = self.ev_tx.clone();
        tokio::task::spawn_blocking(move || {
            let res = files::move_into(&paths, &dir).map_err(|e| e.to_string());
            let _ = ev_tx.send(Ev::CarryDropped { id, dir, res });
        });
    }

    fn on_carry_fetched(&mut self, id: u64, paths: Vec<PathBuf>) {
        if let Some(dir) = self.drops.remove(&id) {
            return self.finish_drop(id, paths, dir);
        }
        match self.carry.as_mut() {
            Some(c) if c.id == id => c.staged = Some(paths),
            _ => {
                // Nobody wants them any more (the cursor went back).
                discard_staged(&paths);
                self.sh.remove_transfer(id, "in");
            }
        }
    }

    /// Calls off the files we sent over with a drag: the transfer row goes away and the
    /// offer can no longer be fetched.
    fn cancel_outgoing(&mut self) {
        let Some(id) = self.held.as_ref().map(|h| h.offer) else {
            return;
        };
        self.called_off.insert(id);
        if self.called_off.len() > 32 {
            self.called_off.clear();
        }
        self.sh.offers.lock().unwrap().retain(|(i, _)| *i != id);
        self.sh.remove_transfer(id, "out");
    }

    /// Keeps `left_down` in step with the mouse input we forward or receive. A new press
    /// starts a new gesture, so files held from an earlier one are forgotten.
    fn track_button(&mut self, ev: &InputEvent) {
        if let InputEvent::Button {
            button: Button::Left,
            down,
        } = ev
        {
            self.left_down = *down;
            if *down {
                self.held = None;
                self.holding_local = false;
                self.sync_carry();
            } else if !self.holding_local {
                self.held = None;
            }
        }
    }

    /// The button went up while our own files were in hand: they land in the folder under
    /// the cursor (nothing happens when that is the folder they came from).
    fn drop_held_locally(&mut self) {
        let held = self.held.take();
        self.holding_local = false;
        self.sync_carry();
        let Some(h) = held else { return };
        let (x, y) = self.sh.platform.cursor_pos();
        let Some(dir) = self.sh.platform.drop_dir_at(x, y) else {
            return;
        };
        if h.paths.first().and_then(|p| p.parent()) == Some(dir.as_path()) {
            return;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.notify(&format!("Dropped into {name}"));
        tokio::task::spawn_blocking(move || {
            let _ = files::move_or_copy(&h.paths, &dir);
        });
    }

    /// Tells the UI whether files are being carried to this computer right now.
    fn sync_carry(&self) {
        let view = self.carry.as_ref().map(|c| c.view.clone()).or_else(|| {
            if self.holding_local {
                self.held.as_ref().map(|h| h.view.clone())
            } else {
                None
            }
        });
        self.sh.platform.set_carrying(view.is_some());
        self.sh.status.lock().unwrap().carry = view;
        self.sh.publish();
    }

    /// The mouse left (or the link dropped) before the files were let go: forget them.
    fn cancel_carry(&mut self) {
        let taken = self.carry.take();
        self.sync_carry();
        if let Some(c) = taken {
            if let Some(f) = &c.fetch {
                f.abort(); // stop downloading what nobody wants any more
            }
            if let (None, Some(dir)) = (&c.staged, c.dir.clone()) {
                tokio::task::spawn_blocking(move || {
                    let _ = std::fs::remove_dir_all(dir);
                });
            }
            if let Some(paths) = &c.staged {
                discard_staged(paths);
            }
            self.sh.remove_transfer(c.id, "in");
        }
    }
}

/// What the little picture under the cursor shows for these files.
fn carry_view(paths: &[PathBuf]) -> CarryView {
    let first = paths.first();
    CarryView {
        name: first
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        count: paths.len(),
        folder: first.is_some_and(|p| p.is_dir()),
    }
}

/// Deletes carried files that were never dropped (and their staging folder).
fn discard_staged(paths: &[PathBuf]) {
    let mut parent = None;
    for p in paths {
        if parent.is_none() {
            parent = p.parent().map(std::path::Path::to_path_buf);
        }
        let _ = if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
    }
    if let Some(d) = parent {
        let _ = std::fs::remove_dir(d);
    }
}

async fn fetch_files(
    sh: &Arc<Shared>,
    addr: SocketAddr,
    key: [u8; 32],
    id: u64,
    items: &[FileItem],
    dest: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connection timed out",
            ))
        })??;
    stream.set_nodelay(true)?;
    let (_, (mut r, mut w)) = tokio::time::timeout(
        Duration::from_secs(10),
        client_handshake(
            stream,
            &sh.me,
            Purpose::Files,
            ClientAuth::Known { link_key: key },
        ),
    )
    .await
    .map_err(|_| Error::Auth("handshake timed out".into()))??;
    let sh2 = sh.clone();
    let prog = move |n: u64| sh2.transfer_progress(id, "in", n);
    files::receive(&mut r, &mut w, id, items, dest, &prog).await
}

fn is_mac(s: &str) -> bool {
    parse_mac(s).is_some()
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut m = [0u8; 6];
    for (o, p) in m.iter_mut().zip(parts) {
        if p.len() != 2 {
            return None;
        }
        *o = u8::from_str_radix(p, 16).ok()?;
    }
    (m != [0; 6] && m != [0xff; 6]).then_some(m)
}

/// The "magic packet" that wakes a sleeping computer: six 0xFF then its address 16
/// times, broadcast on the local network (and sent to where it was last seen).
pub fn wake_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut p = vec![0xFFu8; 6];
    for _ in 0..16 {
        p.extend_from_slice(&mac);
    }
    p
}

fn send_wake(mac: [u8; 6], last: Option<IpAddr>) {
    let pkt = wake_packet(mac);
    let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return;
    };
    let _ = sock.set_broadcast(true);
    for _ in 0..3 {
        let _ = sock.send_to(&pkt, "255.255.255.255:9");
        if let Some(IpAddr::V4(v4)) = last {
            // The subnet's own broadcast address too (some routers drop 255.255.255.255).
            let o = v4.octets();
            let _ = sock.send_to(&pkt, (std::net::Ipv4Addr::new(o[0], o[1], o[2], 255), 9));
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    log::info!("wake: magic packet sent");
}

#[cfg(test)]
mod wake_tests {
    use super::*;

    #[test]
    fn the_wake_packet_and_addresses() {
        let m = parse_mac("AA:bb:0c:dd:ee:0F").unwrap();
        assert_eq!(m, [0xaa, 0xbb, 0x0c, 0xdd, 0xee, 0x0f]);
        assert_eq!(parse_mac("aa-bb-0c-dd-ee-0f"), Some(m));
        assert!(parse_mac("00:00:00:00:00:00").is_none());
        assert!(parse_mac("zz:bb:0c:dd:ee:0f").is_none());
        assert!(parse_mac("aa:bb:cc").is_none());
        let p = wake_packet(m);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xff; 6]);
        assert_eq!(&p[6..12], &m);
        assert_eq!(&p[96..], &m);
    }

    #[test]
    fn colours_are_stable() {
        assert_eq!(device_rgb("abc"), device_rgb("abc"));
        assert!(PALETTE.contains(&device_rgb("0123456789abcdef")));
        assert_eq!(css([0x2d, 0xd4, 0xbf]), "#2dd4bf");
    }
}
