# Tandem

Use **one mouse and keyboard on two Windows computers**. Push the cursor off the
edge of one screen and it appears on the other; copy on one, paste on the other
(text *and* files); drop files on the window to send them.

Built on the same stack as Coucou's Windows app (Tauri 2 + Rust + TypeScript),
with its own name, icon and look. No account, no cloud, no telemetry — the two
computers talk directly on your local network.

```
crates/tandem-core   protocol, encryption, pairing, discovery, engine, Windows input layer (src/win.rs)
crates/tandem-cli    `tandem` — same engine from a terminal (debugging, headless use)
src-tauri + src      the desktop app (tray + window)
```

## Using it

1. Install and start Tandem on both computers (it lives in the tray; closing the window keeps it running).
2. On computer **B**, find computer **A** under *Nearby*, type the 6-digit code A shows, press *Pair*. Once.
3. Tell Tandem where the other computer sits ("Place right", etc.).
4. Push the cursor through that screen edge. Come back the same way.

**Scroll Lock** jumps to the other computer from anywhere — and, while you're
on the other computer, brings your mouse straight back. If anything ever feels
stuck, press it.

Windows Firewall will ask once; allow Tandem on *Private* networks (it listens on TCP 24800 and UDP 24801 for discovery).

## How it works / security

* **Pairing:** SPAKE2 password-authenticated key exchange on the 6-digit code. A
  sniffer learns nothing offline; a wrong guess burns the code after 3 tries and a
  new one is generated. After pairing both sides keep a 256-bit link key
  (Windows Credential Manager) and reconnect with an HMAC challenge, no code needed.
* **Transport:** everything is ChaCha20-Poly1305 with per-direction keys and counters.
* **Trust model:** a paired computer can type and click on yours while it has control —
  that's the feature. "Forget" removes it. Pair only machines you own.
* **Clipboard:** text and copied files sync both ways while connected. Clipboard
  items flagged by password managers as sensitive are never synced. Turn the whole
  thing off in Settings.
* **Files:** received paths are validated (no `..`, drive letters, reserved names,
  etc.), symlinks are never followed when sending, and copied-file sync stops at
  512 MB (use *Send files* for bigger things).
* **Input:** your physical input is swallowed locally only while the other
  computer has control; a heartbeat gives the mouse back within ~5 s if the link dies.

## Build

Needs Rust, Node 20+, and (for the app) WebView2 (already on Windows 10/11).

```
npm install
npx tauri build          # installer in target/release/bundle/nsis
cargo test               # core + CLI, runs on any OS
cargo run -p tandem-cli  # terminal version; `help` for commands
```

On non-Windows systems the input layer is an in-memory stand-in, so pairing,
clipboard and file transfer can be developed and tested anywhere
(`npm run dev` in a browser even uses a mocked backend: `#offline`, `#connected`, `#remote`, `#controlled`).

## The top bar (Bob)

At rest the bar is **one thin line** at the top centre of your main screen. It takes no
clicks and never pops up by itself; its colour shows the state (grey: not connected,
green: connected, teal: you are controlling the other PC, amber: it is controlling you,
pulsing teal: a transfer is running).

- **Ctrl+Alt+T** opens the control bar (Bob, status, which side the other PC is on, Jump /
  Disconnect). Press it again to close. It also closes by itself after a while.
- **Ctrl+Alt+U** opens or closes the small transfer bar (progress of what is being sent or
  received). It appears by itself while a transfer runs; close it by hand with the same key.
- **Send a file by dragging:** pick it up in Explorer or on the desktop, press **Space**, drop
  it on the bar. Let go anywhere else and the bar closes again. Nothing opens just because you
  dragged something, so moving desktop icons is unaffected.
- **Right-click → "Send with Tandem"** on any file or folder (switch it on in the settings).
- **Copy files with Ctrl+C** on one PC and **paste with Ctrl+V** in Explorer on the other
  (up to 512 MB; larger ones: use Send).
- In a **full-screen app** the line hides too. The two hotkeys still work there and the bar
  goes away again when you close it.
- No AI features — those were removed from the Coucou code.

Bob, the sounds and the animations are original and made in code. Parts of the island
code are adapted from Coucou (MIT) — see THIRD-PARTY-NOTICES.md.

## Newer features

- **Game mode** — no edge switching while a full-screen app is open (Scroll Lock still jumps).
- **Hold at the edge** — optional delay before the cursor crosses, against accidental switches.
- **Copied images** — screenshots (Win+Shift+S) and "copy image" travel with the clipboard
  as PNG. Needs the app that copied to offer the PNG format (Snipping Tool, browsers do).
- **File sends slow down while you are controlling the other PC**, so big transfers never
  make the mouse lag.
- Top-bar settings (display, sounds, collapse delay) live in the panel.

## Status — read this

Verified automatically (Linux): handshake/pairing, wrong-code rejection, encrypted
framing, edge crossing in both directions, key/mouse forwarding, hand-back,
failsafe, disconnect handling, clipboard text, copied files and folders, explicit
file send with name-clash handling, path sanitising — including a run with two
separate `tandem` processes over TCP.

**Not yet exercised on real Windows hardware:** `crates/tandem-core/src/win.rs`
(mouse/keyboard hooks, `SendInput`, clipboard) and the Tauri shell were written
against the real `windows`/`tauri` crate sources but could not be compiled or run
in the environment they were written in. The CI `windows` job compiles them; the
first real test is on two PCs. Suggested order: `tandem-cli` on both (proves the
hooks without any UI), then the app.

## Known limits (v1)

* One paired computer connected at a time; LAN only (no internet relay).
* Elevated (admin) windows and the UAC / lock screen ignore injected input unless
  Tandem itself runs elevated; Ctrl+Alt+Del can't be forwarded.
* Games using raw input may not see injected mouse motion.
* Pointer position is mapped by screen *fraction* when crossing; mixed-DPI
  multi-monitor layouts use the bounding box of the whole desktop.
* The first time the cursor touches the edge it must keep pushing for a few
  events before it crosses (so scrollbars and window borders at the edge stay usable);
  dragging with a button held never crosses.
* The installer is unsigned, so SmartScreen will warn.

The island's Rust side (`src-tauri/src/island.rs`, window wiring in `lib.rs`) is written
but has not been compiled or run: this environment has no Windows target. The island UI
itself was checked in a browser with a mock backend. Expect to fix small compile or
click-through issues on first `npm run tauri dev`.

Dropped files arrive in the page as normal HTML5 file drops, are copied chunk by chunk
into a temporary staging folder, and are sent from there. If something looks wrong,
`tandem.log` in `%APPDATA%\dev.tandem.app\` records the island's start-up and any page errors.

## Several computers (hub)

One PC is the hub. Up to **4 other PCs** can connect to it, one per side of its screen (right, left, top, bottom). Pair each one with the usual code; Tandem gives every new PC the first free side. Rearrange them in settings by clicking a computer, then clicking the side it should sit on.

- Push the cursor through an edge to jump to the computer on that side. The bar and settings also have per-computer **Jump** and **Disconnect** buttons.
- Text and image clipboard is relayed through the hub to every computer.
- Files: drop on a computer's name on the bar, or use "Send to" in settings. Right-click "Send with Tandem" goes to the computer you are controlling, or to all connected ones.
- A copied-files clipboard (Ctrl+C / Ctrl+V) is not relayed between two spokes. Send those through the hub.
- A 5th computer is turned away.
- Same network only (Windows only). Internet relay and macOS/Linux ports are not built yet.

## Hidden pointer, admin windows

- While you drive another computer, the pointer on this one is hidden (it would otherwise sit
  stuck in the middle of the screen) and comes back when control returns. If Tandem ever
  crashes while it is hidden, it restores your cursors on the next start.
- Windows ignores a normal program's mouse and keyboard in windows that run as administrator.
  Settings → *Admin windows* has **Restart as administrator** (one question from Windows) and
  **Always start as administrator**. Do this on the computer you control. Trade-off: in
  administrator mode you can't drag files from Explorer onto Tandem, and the right-click
  "Send with Tandem" won't reach it.
- The dimmed UAC prompt and the lock screen can't be controlled by any program. When one
  appears, Tandem hands the mouse and keyboard back automatically so you can answer it on that
  computer.
