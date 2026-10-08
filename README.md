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
