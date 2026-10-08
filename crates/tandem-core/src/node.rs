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
use std::collections::{HashMap, VecDeque};
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
}

#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct Snapshot {
    pub device_id: String,
    pub device_name: String,
    /// Shown as `123-456`; the other computer types it to pair.
    pub pairing_code: String,
    pub tcp_port: u16,
    /// "offline" | "local" | "remote" | "controlled"
    pub control: String,
    pub connected: Option<PeerBrief>,
    pub paired: Vec<PairedView>,
    pub nearby: Vec<NearbyView>,
    pub transfers: Vec<TransferView>,
    pub clipboard_sync: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------- shared state

struct PairingState {
    code: String,
    attempts_left: u8,
}

#[derive(Default)]
struct Status {
    control: &'static str,
    connected: Option<PeerBrief>,
    error: Option<String>,
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
        let connected_id = st.connected.as_ref().map(|c| c.id.clone());
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
            control: if st.connected.is_some() {
                st.control.to_string()
            } else {
                "offline".into()
            },
            connected: st.connected.clone(),
            paired: per
                .peers
                .iter()
                .map(|p| PairedView {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    side: p.side,
                    connected: connected_id.as_deref() == Some(&p.id),
                    nearby: disc.get(&p.id).map(fresh).unwrap_or(false),
                })
                .collect(),
            nearby,
            transfers: self.transfers.lock().unwrap().clone(),
            clipboard_sync: per.clipboard_sync,
            error: st.error.clone(),
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
        let mut v = self.transfers.lock().unwrap();
        if let Some(x) = v.iter_mut().find(|x| x.id == id && x.direction == dir) {
            x.bytes = bytes;
            x.state = "active".into();
        }
        drop(v);
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
    Disconnect,
    SetSide {
        peer_id: String,
        side: Edge,
    },
    Forget(String),
    SendFiles(Vec<PathBuf>),
    SetClipboardSync(bool),
    SetName(String),
    NewCode,
    Engage,
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
            session: None,
            dialing_since: None,
            last_dial: HashMap::new(),
            last_clip_sig: None,
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
    pub fn disconnect(&self) {
        self.cmd(Cmd::Disconnect)
    }
    pub fn set_side(&self, peer_id: String, side: Edge) {
        self.cmd(Cmd::SetSide { peer_id, side })
    }
    pub fn forget(&self, peer_id: String) {
        self.cmd(Cmd::Forget(peer_id))
    }
    pub fn send_files(&self, paths: Vec<PathBuf>) {
        self.cmd(Cmd::SendFiles(paths))
    }
    pub fn set_clipboard_sync(&self, on: bool) {
        self.cmd(Cmd::SetClipboardSync(on))
    }
    pub fn set_name(&self, name: String) {
        self.cmd(Cmd::SetName(name))
    }
    pub fn new_pairing_code(&self) {
        self.cmd(Cmd::NewCode)
    }
    /// Same as the hotkey: jump to the other computer now.
    pub fn engage(&self) {
        self.cmd(Cmd::Engage)
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
        res: std::io::Result<Offer>,
    },
    ClipFiles(Vec<PathBuf>),
    DialFailed(String),
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
        for m in out {
            if w.send(&m).await.is_err() {
                return;
            }
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum Control {
    Local,
    /// We are swallowing local input and forwarding it, through `edge`.
    Remote {
        edge: Edge,
    },
    /// The peer controls our cursor; `ret` is our edge that leads back.
    Controlled {
        ret: Edge,
    },
}

struct Session {
    sid: u64,
    peer: PeerInfo,
    addr: IpAddr,
    tx: UnboundedSender<Msg>,
    reader: AbortHandle,
    last_rx: Instant,
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
    session: Option<Session>,
    dialing_since: Option<Instant>,
    last_dial: HashMap<String, Instant>,
    last_clip_sig: Option<u64>,
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
        self.control = c;
        self.sh.status.lock().unwrap().control = match c {
            Control::Local => "local",
            Control::Remote { .. } => "remote",
            Control::Controlled { .. } => "controlled",
        };
        self.sh.publish();
    }

    fn send(&self, m: Msg) {
        if let Some(s) = &self.session {
            let _ = s.tx.send(m);
        }
    }

    fn center(&self) -> (i32, i32) {
        self.sh.platform.screen().center()
    }

    fn drop_session(&mut self, why: &str) {
        if self.session.is_none() {
            return;
        }
        match self.control {
            Control::Remote { .. } => {
                let (x, y) = self.center();
                self.sh.platform.end_capture(x, y);
            }
            Control::Controlled { .. } => self.sh.platform.release_all(),
            Control::Local => {}
        }
        self.session = None;
        self.set_control(Control::Local);
        self.sh.platform.set_active_edge(None);
        {
            let mut st = self.sh.status.lock().unwrap();
            st.connected = None;
            if !why.is_empty() {
                st.error = Some(why.to_string());
            }
        }
        self.sh.publish();
    }

    fn on_tick(&mut self) {
        if let Some(s) = &self.session {
            let _ = s.tx.send(Msg::Ping);
            if s.last_rx.elapsed() > SESSION_TIMEOUT {
                self.drop_session("connection lost");
            }
        }
        if self
            .dialing_since
            .is_some_and(|t| t.elapsed() > Duration::from_secs(20))
        {
            self.dialing_since = None;
        }
        self.auto_connect();
        self.sh.publish(); // lets "nearby" entries expire
    }

    /// Reconnect to paired computers. Only the device with the smaller id
    /// dials, so two computers don't dial each other at the same time.
    fn auto_connect(&mut self) {
        if self.session.is_some() || self.dialing_since.is_some() {
            return;
        }
        let per = self.sh.persisted.lock().unwrap().clone();
        for p in &per.peers {
            if self.sh.me.id >= p.id {
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
                self.last_dial.insert(p.id.clone(), Instant::now());
                self.start_dial(
                    SocketAddr::new(ip, port),
                    ClientAuth::Known { link_key: key },
                );
                return;
            }
        }
    }

    fn start_dial(&mut self, addr: SocketAddr, auth: ClientAuth) {
        self.dialing_since = Some(Instant::now());
        let (sh, ev_tx) = (self.sh.clone(), self.ev_tx.clone());
        tokio::spawn(async move {
            if let Err(e) = dial(sh, ev_tx.clone(), addr, auth).await {
                let _ = ev_tx.send(Ev::DialFailed(e.to_string()));
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
                if self.session.as_ref().map(|s| s.sid) == Some(sid) {
                    self.session.as_mut().unwrap().last_rx = Instant::now();
                    self.on_msg(m);
                }
            }
            Ev::Closed(sid) => {
                if self.session.as_ref().map(|s| s.sid) == Some(sid) {
                    self.drop_session("disconnected");
                }
            }
            Ev::Cmd(c) => self.on_cmd(c),
            Ev::DialFailed(e) => {
                self.dialing_since = None;
                // Quiet during background reconnects; loud only when nothing else is going on.
                log::info!("dial failed: {e}");
                self.sh.set_error(Some(e));
            }
            Ev::OfferBuilt { id, kind, res } => self.on_offer_built(id, kind, res),
            Ev::ClipFiles(paths) => {
                let c = ClipContent::Files(paths);
                self.last_clip_sig = Some(sig(&c));
                self.sh.platform.clipboard_set(&c);
            }
        }
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
        self.dialing_since = None;
        // One computer at a time; a second connection from the same peer replaces a dead one only.
        if self.session.is_some() {
            reader.abort();
            return;
        }
        // Persist new pairing / refresh name & address.
        let side = {
            let mut per = self.sh.persisted.lock().unwrap();
            let sock = SocketAddr::new(addr, peer.listen_port).to_string();
            if let Some(p) = per.peers.iter_mut().find(|p| p.id == peer.id) {
                p.name = peer.name.clone();
                p.last_addr = Some(sock);
                if let Some(k) = peer.new_link_key {
                    p.link_key = hex(&k);
                }
            } else if let Some(k) = peer.new_link_key {
                per.peers.push(PairedPeer {
                    id: peer.id.clone(),
                    name: peer.name.clone(),
                    link_key: hex(&k),
                    side: Edge::Right,
                    last_addr: Some(sock),
                });
            } else {
                // Known-mode connection for a peer we no longer have: cannot happen (policy checked).
                reader.abort();
                return;
            }
            per.peers.iter().find(|p| p.id == peer.id).unwrap().side
        };
        self.sh.save();
        self.session = Some(Session {
            sid,
            peer: peer.clone(),
            addr,
            tx,
            reader,
            last_rx: Instant::now(),
            side,
        });
        {
            let mut st = self.sh.status.lock().unwrap();
            st.connected = Some(PeerBrief {
                id: peer.id.clone(),
                name: peer.name.clone(),
            });
            st.error = None;
        }
        self.sh.discovered.lock().unwrap().remove(&peer.id);
        self.set_control(Control::Local);
        self.sh.platform.set_active_edge(Some(side));
        if dialer {
            self.send(Msg::Layout { peer_side: side });
        }
    }

    fn side(&self) -> Option<Edge> {
        self.session.as_ref().map(|s| s.side)
    }

    fn on_capture(&mut self, c: CaptureEvent) {
        match c {
            CaptureEvent::Crossed { edge, frac } => {
                if self.control == Control::Local && self.side() == Some(edge) {
                    self.set_control(Control::Remote { edge });
                    self.send(Msg::Enter { edge, frac });
                } else {
                    // Raced with a disconnect or a layout change: give the user their mouse back.
                    let (x, y) = self.center();
                    self.sh.platform.end_capture(x, y);
                }
            }
            CaptureEvent::Input(ev) => {
                if matches!(self.control, Control::Remote { .. }) {
                    self.send(Msg::Input(ev));
                }
            }
            CaptureEvent::Failsafe => {
                if matches!(self.control, Control::Remote { .. }) {
                    self.set_control(Control::Local);
                    self.send(Msg::Release);
                }
            }
            CaptureEvent::HotkeyEngage => self.engage(),
            CaptureEvent::ClipboardChanged => self.on_clipboard_changed(),
        }
    }

    fn engage(&mut self) {
        if let (Control::Local, Some(edge)) = (self.control, self.side()) {
            self.sh.platform.begin_capture();
            self.set_control(Control::Remote { edge });
            self.send(Msg::Enter { edge, frac: 0.5 });
        }
    }

    fn on_msg(&mut self, m: Msg) {
        let plat = self.sh.platform.clone();
        match m {
            Msg::Ping => self.send(Msg::Pong),
            Msg::Pong => {}
            Msg::Bye => self.drop_session("peer closed the connection"),
            Msg::Layout { peer_side } => {
                let mine = peer_side.opposite();
                self.apply_side(mine, false);
            }
            Msg::Enter { edge, frac } => {
                if self.control == Control::Local {
                    let (x, y) = edge_point(plat.screen(), edge.opposite(), frac);
                    plat.set_cursor(x, y);
                    self.set_control(Control::Controlled {
                        ret: edge.opposite(),
                    });
                } else {
                    self.send(Msg::Release); // "not now"
                }
            }
            Msg::Leave { frac } => {
                if let Control::Remote { edge } = self.control {
                    let (x, y) = edge_point(plat.screen(), edge, frac);
                    plat.end_capture(x, y);
                    self.set_control(Control::Local);
                }
            }
            Msg::Release => match self.control {
                Control::Remote { .. } => {
                    let (x, y) = self.center();
                    plat.end_capture(x, y);
                    self.set_control(Control::Local);
                }
                Control::Controlled { .. } => {
                    plat.release_all();
                    self.set_control(Control::Local);
                }
                Control::Local => {}
            },
            Msg::Input(ev) => {
                if let Control::Controlled { ret } = self.control {
                    plat.inject(&ev);
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
                        };
                        if let Some(frac) = leaving {
                            plat.release_all();
                            self.set_control(Control::Local);
                            self.send(Msg::Leave { frac });
                        }
                    }
                }
            }
            Msg::ClipText(t) => {
                let on = self.sh.persisted.lock().unwrap().clipboard_sync;
                if on && t.len() <= MAX_CLIP_TEXT {
                    let c = ClipContent::Text(t);
                    self.last_clip_sig = Some(sig(&c));
                    plat.clipboard_set(&c);
                }
            }
            Msg::FileOffer { id, kind, items } => self.on_file_offer(id, kind, items),
        }
    }

    fn apply_side(&mut self, side: Edge, announce: bool) {
        let Some(sess) = self.session.as_mut() else {
            return;
        };
        sess.side = side;
        let pid = sess.peer.id.clone();
        {
            let mut per = self.sh.persisted.lock().unwrap();
            if let Some(p) = per.peers.iter_mut().find(|p| p.id == pid) {
                p.side = side;
            }
        }
        self.sh.save();
        if self.control == Control::Local {
            self.sh.platform.set_active_edge(Some(side));
        }
        if announce {
            self.send(Msg::Layout { peer_side: side });
        }
        self.sh.publish();
    }

    fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Connect { addr, port, code } => {
                if self.session.is_none() {
                    self.sh.set_error(None);
                    self.start_dial(SocketAddr::new(addr, port), ClientAuth::Pair { code });
                }
            }
            Cmd::ConnectKnown { peer_id } => {
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
                if let (Some((ip, port)), Some(key), true) =
                    (target, unhex32(&p.link_key), self.session.is_none())
                {
                    self.sh.set_error(None);
                    self.start_dial(
                        SocketAddr::new(ip, port),
                        ClientAuth::Known { link_key: key },
                    );
                }
            }
            Cmd::Disconnect => {
                self.send(Msg::Bye);
                self.drop_session("");
            }
            Cmd::SetSide { peer_id, side } => {
                if self.session.as_ref().map(|s| s.peer.id == peer_id) == Some(true) {
                    self.apply_side(side, true);
                } else {
                    let mut per = self.sh.persisted.lock().unwrap();
                    if let Some(p) = per.peers.iter_mut().find(|p| p.id == peer_id) {
                        p.side = side;
                    }
                    drop(per);
                    self.sh.save();
                    self.sh.publish();
                }
            }
            Cmd::Forget(id) => {
                if self.session.as_ref().map(|s| s.peer.id == id) == Some(true) {
                    self.send(Msg::Bye);
                    self.drop_session("");
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
            Cmd::Engage => self.engage(),
            Cmd::SendFiles(paths) => {
                if self.session.is_some() {
                    self.build_offer(paths, OfferKind::Send);
                } else {
                    self.sh
                        .set_error(Some("Connect to a computer first".into()));
                }
            }
        }
    }

    // ---- clipboard & files

    fn on_clipboard_changed(&mut self) {
        let on = self.sh.persisted.lock().unwrap().clipboard_sync;
        if !on || self.session.is_none() {
            return;
        }
        let Some(c) = self.sh.platform.clipboard_get() else {
            return;
        };
        let s = sig(&c);
        if self.last_clip_sig == Some(s) {
            return; // our own echo, or unchanged
        }
        self.last_clip_sig = Some(s);
        match c {
            ClipContent::Text(t) => {
                if !t.is_empty() && t.len() <= MAX_CLIP_TEXT {
                    self.send(Msg::ClipText(t));
                }
            }
            ClipContent::Files(paths) => self.build_offer(paths, OfferKind::Clipboard),
        }
    }

    fn build_offer(&mut self, paths: Vec<PathBuf>, kind: OfferKind) {
        let id: u64 = rand::thread_rng().gen();
        let ev_tx = self.ev_tx.clone();
        tokio::task::spawn_blocking(move || {
            let res = files::build_manifest(&paths);
            let _ = ev_tx.send(Ev::OfferBuilt { id, kind, res });
        });
    }

    fn on_offer_built(&mut self, id: u64, kind: OfferKind, res: std::io::Result<Offer>) {
        let offer = match res {
            Ok(o) if !o.items.is_empty() => Arc::new(o),
            Ok(_) => return,
            Err(e) => return self.sh.set_error(Some(format!("Cannot send files: {e}"))),
        };
        if self.session.is_none() {
            return;
        }
        {
            let mut q = self.sh.offers.lock().unwrap();
            q.push_back((id, offer.clone()));
            while q.len() > 8 {
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
        });
        self.send(Msg::FileOffer {
            id,
            kind,
            items: offer.items.clone(),
        });
    }

    fn on_file_offer(&mut self, id: u64, kind: OfferKind, items: Vec<FileItem>) {
        let Some(sess) = &self.session else { return };
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
        let Some(key) = unhex32(
            &per.peers
                .iter()
                .find(|p| p.id == sess.peer.id)
                .map(|p| p.link_key.clone())
                .unwrap_or_default(),
        ) else {
            return;
        };
        let dest = match kind {
            OfferKind::Clipboard => self.sh.cfg.temp_dir.join(format!("clip-{id:016x}")),
            OfferKind::Send => self.sh.cfg.download_dir.clone(),
        };
        self.sh.upsert_transfer(view("active", ""));
        let addr = SocketAddr::new(sess.addr, sess.peer.listen_port);
        let (sh, ev_tx) = (self.sh.clone(), self.ev_tx.clone());
        tokio::spawn(async move {
            let res = fetch_files(&sh, addr, key, id, &items, &dest).await;
            match res {
                Ok(paths) => {
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
