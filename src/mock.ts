// Browser-only stand-in for the Rust side, so the UI can be developed (and
// screenshotted) without Tauri. Pick a scenario with the URL hash:
//   #offline  #connected  #remote  #controlled
import type { Snapshot } from "./types";

const scenario = (location.hash || "#offline").slice(1);
const base: Snapshot = {
  device_id: "a1b2c3d4e5f60718",
  device_name: "DESKTOP-WORK",
  pairing_code: "802-771",
  tcp_port: 24800,
  control: "offline",
  connected: null,
  paired: [],
  nearby: [
    { id: "n1", name: "LAPTOP-HOME", addr: "192.168.1.42", port: 24800, paired: false },
    { id: "n2", name: "Studio-PC", addr: "192.168.1.57", port: 24800, paired: true },
  ],
  transfers: [],
  clipboard_sync: true,
  error: null,
};
let state: Snapshot = { ...base };
if (scenario !== "offline") {
  state = {
    ...base,
    control: scenario === "connected" ? "local" : (scenario as Snapshot["control"]),
    connected: { id: "n2", name: "Studio-PC" },
    paired: [{ id: "n2", name: "Studio-PC", side: "Right", connected: true, nearby: true }],
    nearby: [],
    transfers: [
      { id: 1, name: "holiday-photos", direction: "out", bytes: 48_000_000, total: 120_000_000, state: "active", message: "", path: null },
      { id: 2, name: "invoice.pdf", direction: "in", bytes: 220_000, total: 220_000, state: "done", message: "", path: "C:\\Users\\me\\Downloads\\Tandem" },
      { id: 3, name: "movie.mkv", direction: "in", bytes: 0, total: 4_000_000_000, state: "error", message: "Copied files are too large to sync automatically — use Send files", path: null },
    ],
  };
}
const listeners: ((s: Snapshot) => void)[] = [];
const push = () => listeners.forEach((l) => l({ ...state }));

export function onSnapshot(cb: (s: Snapshot) => void) {
  listeners.push(cb);
  setTimeout(() => cb({ ...state }), 0);
}

export async function call(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  console.log("[mock]", cmd, args);
  switch (cmd) {
    case "snapshot":
      return { ...state };
    case "set_side": {
      const s = String(args?.side);
      state.paired = state.paired.map((p) => ({ ...p, side: (s[0].toUpperCase() + s.slice(1)) as never }));
      push();
      return;
    }
    case "autostart_get":
      return true;
    case "set_clipboard_sync":
      state.clipboard_sync = !!args?.on;
      push();
      return;
    case "connect":
      state.error = "Wrong code, or that computer isn't accepting new devices";
      push();
      return;
    default:
      return;
  }
}
