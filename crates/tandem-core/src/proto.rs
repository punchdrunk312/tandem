//! Wire protocol. Everything after the handshake is `postcard`-encoded and
//! encrypted; see `channel.rs`.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 2;
/// UDP port used for "I'm here" broadcasts on the LAN.
pub const DISCOVERY_PORT: u16 = 24801;
/// Default TCP port for the control/file listener.
pub const DEFAULT_TCP_PORT: u16 = 24800;
pub const MAGIC: [u8; 4] = *b"TNDM";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Relative mouse motion in pixels.
    MouseMove {
        dx: i32,
        dy: i32,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// Wheel deltas in Windows units (120 = one notch).
    Wheel {
        dx: i32,
        dy: i32,
    },
    /// Windows virtual-key + hardware scan code.
    Key {
        vk: u16,
        scan: u16,
        extended: bool,
        down: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileItem {
    /// Relative path, always `/`-separated. Sanitised again on receipt.
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfferKind {
    /// Files were copied (Ctrl+C); the receiver puts them on its clipboard.
    Clipboard,
    /// Files were explicitly sent; the receiver saves them to a folder.
    Send,
}

/// Messages on the control connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    /// "My peer sits on this side of my screen" — the receiver adopts the opposite.
    Layout {
        peer_side: Edge,
    },
    /// The sender's cursor left through `edge`, at `frac` (0..1) along it.
    /// The receiver now owns the cursor.
    Enter {
        edge: Edge,
        frac: f32,
    },
    /// The controlled side's cursor went back through the edge it came in by.
    Leave {
        frac: f32,
    },
    /// The sender stops controlling (hotkey / failsafe). Receiver releases keys.
    Release,
    Input(InputEvent),
    ClipText(String),
    FileOffer {
        id: u64,
        kind: OfferKind,
        items: Vec<FileItem>,
    },
    Ping,
    Pong,
    Bye,
    /// A copied image, as PNG bytes.
    ClipImage(Vec<u8>),
}

/// Messages on a dedicated file connection (receiver dials the sender).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FileMsg {
    Request {
        id: u64,
    },
    /// Starts one item. For directories no data follows.
    Header {
        index: u32,
    },
    Data(Vec<u8>),
    End,
    Done,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purpose {
    Control,
    Files,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode {
    /// First contact: SPAKE2 over the pairing code.
    Pair,
    /// Already paired: challenge/response with the stored link key.
    Known,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub ver: u16,
    pub id: String,
    pub name: String,
    pub listen_port: u16,
    pub purpose: Purpose,
    pub mode: AuthMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerHello {
    Ok {
        id: String,
        name: String,
        listen_port: u16,
        nonce: [u8; 32],
    },
    Reject(String),
}

/// UDP broadcast payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beacon {
    pub magic: [u8; 4],
    pub ver: u16,
    pub id: String,
    pub name: String,
    pub port: u16,
}

pub fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_stdvec(v).expect("serialisable")
}

pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, crate::Error> {
    postcard::from_bytes(b).map_err(|e| crate::Error::Proto(e.to_string()))
}
