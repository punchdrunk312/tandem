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
    pub error: Option<String>,
    /// A short, self-clearing message for the user ("control came back because…").
    pub notice: Option<String>,
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
    carry: Option<CarryView>,
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
                .map(|p| PairedView {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    side: p.side,
                    connected: st.connected.iter().any(|c| c.id == p.id),
                    nearby: disc.get(&p.id).map(fresh).unwrap_or(false),
                })
                .collect(),
            nearby,
            transfers: self.transfers.lock().unwrap().clone(),
            clipboard_sync: per.clipboard_sync,
            game_mode: per.game_mode,
            switch_delay_ms: per.switch_delay_ms,
            drag_files: per.drag_files,
            move_apps: per.move_apps,
            error: st.error.clone(),
            notice: st.notice.as_ref().map(|(m, _)| m.clone()),
            carry: st.carry.clone(),
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
                ..Default::default()
            }),
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
            platform.set_move_apps(p.move_apps);
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
            carry: None,
            drops: HashMap::new(),
            early_release: None,
            held: None,
            called_off: HashSet::new(),
            holding_local: false,
            left_down: false,
            just_left: None,
            pending_app: None,
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
    pub fn set_move_apps(&self, on: bool) {
        self.cmd(Cmd::SetMoveApps(on));
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
        res: std::result::Result<(), String>,
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
    pending_app: Option<(u64, String, AppRef)>,
    /// Clipboard reads / writes run on their own thread: the clipboard can be held by
    /// another app for a moment and a big image takes a while to copy, and the actor
    /// that forwards mouse movement must never wait for either.
    clip: std::sync::mpsc::Sender<ClipJob>,
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

    /// Ends one connection, handing the mouse back if it was involved.
    fn drop_session(&mut self, pid: &str, why: &str) {
        let Some(sess) = self.sessions.remove(pid) else {
            return;
        };
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
        let mut dead = Vec::new();
        for (id, s) in &self.sessions {
            let _ = s.tx.send(Msg::Ping);
            if s.last_rx.elapsed() > SESSION_TIMEOUT {
                dead.push(id.clone());
            }
        }
        for id in dead {
            self.drop_session(&id, "connection lost");
        }
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
                log::info!("dial failed: {err}");
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
                Ok(()) => {
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
                    self.pending_app = Some((id, to.clone(), app.clone()));
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
            },
        );
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
            Msg::Pong => {}
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
                    let (x, y) = edge_point(plat.screen(), edge.opposite(), frac);
                    plat.set_cursor(x, y);
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
                        let (x, y) = edge_point(plat.screen(), edge, frac);
                        plat.end_capture(x, y);
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
            Msg::Release => match self.control.clone() {
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
            },
            Msg::Input(ev) => {
                let ret = match &self.control {
                    Control::Controlled { peer, ret } if peer == from => *ret,
                    _ => return,
                };
                self.track_button(&ev);
                plat.inject(&ev);
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
                    // The other computer's mouse is dragging a program's window here
                    // against the way back: offer to move the program over there instead
                    // of leaving (the cursor stays here so the person can answer).
                    if leaving.is_some()
                        && self.left_down
                        && self.sh.persisted.lock().unwrap().move_apps
                    {
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
                let Some((pid, peer, app)) = self.pending_app.clone() else {
                    return;
                };
                if pid != id || peer != from {
                    return;
                }
                self.pending_app = None;
                let who = self
                    .sessions
                    .get(from)
                    .map(|s| s.peer.name.clone())
                    .unwrap_or_else(|| "the other computer".into());
                if !ok {
                    let m = if note.is_empty() {
                        format!("{} didn't open on {who}", app.name)
                    } else {
                        note
                    };
                    self.notify(&m);
                } else if app.url.is_some() {
                    self.notify(&format!("Opened the page on {who}"));
                } else {
                    let plat = self.sh.platform.clone();
                    let a = app.clone();
                    tokio::task::spawn_blocking(move || plat.close_app(&a));
                    self.notify(&format!("Moved {} to {who}", app.name));
                }
            }
            Msg::FileOffer { id, kind, items } => self.on_file_offer(from, id, kind, items),
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
            Cmd::SetMoveApps(on) => {
                self.sh.persisted.lock().unwrap().move_apps = on;
                self.sh.platform.set_move_apps(on);
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
