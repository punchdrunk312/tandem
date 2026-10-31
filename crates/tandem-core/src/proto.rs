//! Wire protocol. Everything after the handshake is `postcard`-encoded and
//! encrypted; see `channel.rs`.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 12;
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
    /// The four corners: a computer sitting diagonally from this one.
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
            Edge::TopLeft => Edge::BottomRight,
            Edge::BottomRight => Edge::TopLeft,
            Edge::TopRight => Edge::BottomLeft,
            Edge::BottomLeft => Edge::TopRight,
        }
    }

    pub fn is_corner(self) -> bool {
        matches!(
            self,
            Edge::TopLeft | Edge::TopRight | Edge::BottomLeft | Edge::BottomRight
        )
    }

    /// Parses "left", "top-right", "bottomleft", ... (case-insensitive).
    pub fn parse(s: &str) -> Option<Edge> {
        let k: String = s
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Some(match k.as_str() {
            "left" => Edge::Left,
            "right" => Edge::Right,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            "topleft" => Edge::TopLeft,
            "topright" => Edge::TopRight,
            "bottomleft" => Edge::BottomLeft,
            "bottomright" => Edge::BottomRight,
            _ => return None,
        })
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
    /// Files being dragged by the mouse: the receiver holds them until the button is
    /// let go, then drops them in the folder under the cursor.
    Carry,
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
    /// The carried files were let go on this side: the sender can forget them.
    CarryEnd,
    /// A program's window was dragged across the edge: open it here too.
    MoveApp {
        id: u64,
        app: AppRef,
    },
    /// The answer to `MoveApp`: whether it opened here, and why not if it did not.
    MoveAppAnswer {
        id: u64,
        ok: bool,
        note: String,
    },
    /// This computer's licence pass from cubecursor.shop, or none. Sent when the
    /// connection comes up and whenever it changes.
    License {
        pass: Option<String>,
    },
    /// A window is being dragged across the edge towards this computer (see `SpanPos`).
    /// `w` x `h` is its size in pixels.
    SpanBegin {
        w: u32,
        h: u32,
    },
    /// Where the window being dragged across is now. `crossed`: how many pixels of it are
    /// past the edge (on this side). `along`: its top (or left, for a top or bottom edge)
    /// as a fraction of the sender's screen. `gx`, `gy`: the pointer, inside the window.
    SpanPos {
        crossed: i32,
        along: f32,
        gx: i32,
        gy: i32,
    },
    /// A picture of that window (JPEG), a few times a second.
    SpanFrame(Vec<u8>),
    /// The drag ended. `moved`: let go more than halfway over, so the program moves here.
    SpanEnd {
        moved: bool,
    },
    /// How this computer is doing (battery, charging, load, accessories), every few
    /// seconds and whenever something changes.
    Status(DeviceInfo),
    /// Lock or put to sleep the computer this arrives at (a button in the other's bar).
    Power(PowerAction),
    /// "Send me your open programs": the other computer is running low on battery and
    /// the person moves their work over here.
    SendApps,
    /// Second-monitor mode: the window just dragged across stays on the computer that
    /// sends this and is shown live on the one that gets it, from now on as `id`.
    /// `w` x `h`: its size in pixels.
    MirrorStart {
        id: u64,
        w: u32,
        h: u32,
    },
    /// A fresh picture of a mirrored window (JPEG, `w` x `h`: the window's real size).
    MirrorFrame {
        id: u64,
        w: u32,
        h: u32,
        jpeg: Vec<u8>,
    },
    /// The mirrored window is gone (closed, brought home, or this end can't show it).
    MirrorEnd {
        id: u64,
    },
    /// The pointer goes into the mirrored window `id` at (`rx`, `ry`) inside it: the
    /// computer that owns the window takes it from here (sent to that computer).
    PortalEnter {
        id: u64,
        rx: i32,
        ry: i32,
    },
    /// The pointer leaves the mirrored window at (`rx`, `ry`), relative to the window
    /// (just outside it): the computer showing it takes it from here.
    PortalLeave {
        id: u64,
        rx: i32,
        ry: i32,
    },
    /// Where the pointer is inside the mirrored window, so the computer showing it can
    /// draw a pointer there, and its shape (see `span::Shape`: arrow, text beam, hand,
    /// resize arrows…), so it looks exactly like a pointer on a real second monitor.
    PortalCursor {
        id: u64,
        rx: i32,
        ry: i32,
        shape: u8,
    },
    /// The real window was dragged by its title bar by (`dx`, `dy`): the mirror moves
    /// with it. `done`: the drag ended.
    MirrorMoved {
        id: u64,
        dx: i32,
        dy: i32,
        done: bool,
    },
    /// The mirror was dragged back over the edge: the window comes home, at `along`
    /// (0..1) along that edge.
    MirrorHome {
        id: u64,
        along: f32,
    },
    /// Undo: the program that moved over to the computer that gets this goes back.
    UndoApp {
        app: AppRef,
    },
    /// Undo: the files of offer `id` that arrived here go to the Recycle Bin / Trash.
    UndoFiles {
        id: u64,
    },
    /// The size of the sender's screen in pixels (sent when the connection comes up), so
    /// windows crossing between screens of different resolution keep their size on screen.
    Screen {
        w: i32,
        h: i32,
    },
    /// The mirror of window `id` was closed on the computer that showed it (or "bring my
    /// windows back"): the real window comes back on its own computer's screen.
    MirrorClosed {
        id: u64,
    },
}

/// How a computer is doing, as shown on its card in the bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Battery charge in percent; None on a computer without a battery.
    pub battery: Option<u8>,
    /// On the charger.
    pub plugged: bool,
    /// The battery is filling (plugged in and not yet full).
    pub charging: bool,
    /// Minutes the battery lasts at this rate (on battery only), as Windows estimates.
    pub mins_left: Option<u32>,
    /// Minutes until full (while charging), from how fast it has been filling.
    pub mins_to_full: Option<u32>,
    /// Battery saver is on.
    pub saver: bool,
    /// Processor and memory in use, percent.
    pub cpu: u8,
    pub mem: u8,
    /// The system drive, in whole gigabytes.
    pub disk_free_gb: u32,
    pub disk_total_gb: u32,
    /// Minutes since the computer started.
    pub uptime_min: u32,
    /// Wireless mice, keyboards and headphones that report their battery.
    pub accessories: Vec<Accessory>,
    /// The network card's hardware address ("aa:bb:cc:dd:ee:ff"), to wake it up later.
    pub mac: Option<String>,
}

/// A wireless accessory with a battery (earbuds, a headset, a mouse…).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accessory {
    pub name: String,
    /// Percent: the lowest part's for earbuds (what runs out first).
    pub battery: u8,
    /// What it is (see `AccKind`), for its picture.
    #[serde(default)]
    pub kind: AccKind,
    /// Earbuds that tell each part: left, right and the case (percent).
    #[serde(default)]
    pub left: Option<u8>,
    #[serde(default)]
    pub right: Option<u8>,
    #[serde(default)]
    pub case: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccKind {
    #[default]
    Other,
    Earbuds,
    Headphones,
    Mouse,
    Keyboard,
    Gamepad,
    Speaker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerAction {
    Lock,
    Sleep,
}

/// A program being moved to another computer by dragging its window across the edge.
/// Only ever a program's *name*: the other computer finds its own copy and nothing
/// that arrives here can carry a path or a command line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRef {
    /// What people call it ("Discord"): the file name without ".exe".
    pub name: String,
    /// The program's file name only, never a path ("Discord.exe").
    pub exe: String,
    /// For a web browser: the page that was open in the window.
    pub url: Option<String>,
}

/// Programs that must never be launched or moved this way: shells, installers, system
/// parts and CubeCursor itself.
const NEVER_MOVE: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "windowsterminal.exe",
    "wt.exe",
    "conhost.exe",
    "explorer.exe",
    "wscript.exe",
    "cscript.exe",
    "mshta.exe",
    "rundll32.exe",
    "regsvr32.exe",
    "msiexec.exe",
    "reg.exe",
    "regedit.exe",
    "taskmgr.exe",
    "mmc.exe",
    "services.exe",
    "svchost.exe",
    "applicationframehost.exe",
    "systemsettings.exe",
    "dwm.exe",
    "csrss.exe",
    "winlogon.exe",
    "lsass.exe",
    "sihost.exe",
    "bash.exe",
    "wsl.exe",
    "python.exe",
    "pythonw.exe",
    "node.exe",
    "cubecursor.exe",
    "tandem.exe",
    "tandem-cli.exe",
];

/// Web browsers: moving one means opening its page on the other computer, and leaving
/// the browser (with all its other tabs) alone here.
const BROWSERS: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "arc.exe",
    "safari.exe",
];

/// Chat and music programs that are safe to close once they have opened on the other
/// computer (they keep their state in your account, not in the window).
const SAFE_TO_CLOSE: &[&str] = &[
    "discord.exe",
    "spotify.exe",
    "slack.exe",
    "telegram.exe",
    "whatsapp.exe",
    "signal.exe",
    "skype.exe",
    "element.exe",
];

pub fn is_browser_exe(exe: &str) -> bool {
    BROWSERS.contains(&exe.to_ascii_lowercase().as_str())
}

impl AppRef {
    /// Checked on both sides: the sender before it asks, the receiver before it acts.
    pub fn is_sane(&self) -> bool {
        let exe = self.exe.to_ascii_lowercase();
        let plain = |s: &str, max: usize| {
            !s.is_empty()
                && s.len() <= max
                && s.chars().all(|c| {
                    c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '(' | ')' | '+')
                })
        };
        if !plain(&exe, 64) || !exe.ends_with(".exe") || NEVER_MOVE.contains(&exe.as_str()) {
            return false;
        }
        if !plain(&self.name, 64) {
            return false;
        }
        match &self.url {
            None => true,
            Some(u) => {
                is_browser_exe(&exe)
                    && u.len() <= 2048
                    && (u.starts_with("https://") || u.starts_with("http://"))
                    && !u
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace() || c == '"')
            }
        }
    }

    pub fn is_browser(&self) -> bool {
        is_browser_exe(&self.exe)
    }

    /// Whether this program is closed here after it opened on the other computer.
    pub fn may_close(&self) -> bool {
        SAFE_TO_CLOSE.contains(&self.exe.to_ascii_lowercase().as_str())
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;

    fn app(exe: &str, url: Option<&str>) -> AppRef {
        AppRef {
            name: exe.trim_end_matches(".exe").into(),
            exe: exe.into(),
            url: url.map(Into::into),
        }
    }

    #[test]
    fn ordinary_programs_pass_and_dangerous_ones_do_not() {
        assert!(app("Discord.exe", None).is_sane());
        assert!(app("Visual Studio Code.exe", None).is_sane());
        for bad in [
            "cmd.exe",
            "PowerShell.exe",
            "..\\evil.exe",
            "a/b.exe",
            "x.bat",
            "",
            "CubeCursor.exe",
        ] {
            assert!(!app(bad, None).is_sane(), "{bad}");
        }
    }

    #[test]
    fn only_browsers_carry_a_page_and_only_web_addresses() {
        assert!(app("chrome.exe", Some("https://youtu.be/x?t=10")).is_sane());
        assert!(!app("discord.exe", Some("https://example.com")).is_sane());
        for bad in [
            "file:///c:/x",
            "javascript:alert(1)",
            "https://a b",
            "https://a\"b",
            "ftp://x",
        ] {
            assert!(!app("chrome.exe", Some(bad)).is_sane(), "{bad}");
        }
    }

    #[test]
    fn only_chat_and_music_programs_are_closed_afterwards() {
        assert!(app("Discord.exe", None).may_close());
        assert!(app("Spotify.exe", None).may_close());
        assert!(!app("notepad.exe", None).may_close());
        assert!(!app("chrome.exe", Some("https://x.com")).may_close());
    }
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
    /// Instead of `Request`: carry on an interrupted transfer from item `index`, `offset`
    /// bytes in (everything before is already on the receiver's disk).
    Resume {
        id: u64,
        index: u32,
        offset: u64,
    },
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
