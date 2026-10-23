// Browser-only stand-in for the Rust side, so the UI can be developed (and
// screenshotted) without Tauri. Pick a scenario with the URL hash:
//   #offline  #pair  #connected  #remote  #controlled   (+ "&xfer" for transfers)
import type { Snapshot } from "./types";

const hash = (location.hash || "#offline").slice(1);
const [scenario, ...flags] = hash.split("&");
const base: Snapshot = {
  device_id: "a1b2c3d4e5f60718",
  device_name: "DESKTOP-WORK",
  pairing_code: "802-771",
  tcp_port: 24800,
  control: "offline",
  control_peer: null,
  connected: null,
  paired: [],
  nearby: [],
  transfers: [],
  clipboard_sync: true,
  game_mode: true,
  drag_files: true,
  move_apps: false,
  monitor_mode: false,
  carry: null,
  switch_delay_ms: 0,
  error: null,
  notice: null,
  me: { battery: null, plugged: true, charging: false, mins_left: null, mins_to_full: null, saver: false, cpu: 14, mem: 46, disk_free_gb: 412, disk_total_gb: 953, uptime_min: 312, accessories: [{ name: "MX Master 3", battery: 64 }], mac: "aa:bb:cc:00:11:22" },
  my_color: "#2dd4bf",
};
const dev = (o: Partial<NonNullable<Snapshot["me"]>>): NonNullable<Snapshot["me"]> => ({
  battery: 72, plugged: false, charging: false, mins_left: 251, mins_to_full: null, saver: false, cpu: 22, mem: 58,
  disk_free_gb: 118, disk_total_gb: 476, uptime_min: 95, accessories: [], mac: "aa:bb:cc:00:11:33", ...o,
});
let state: Snapshot = { ...base };
if (scenario === "pair") {
  state.nearby = [{ id: "n1", name: "LAPTOP-HOME", addr: "192.168.1.42", port: 24800, paired: false }];
} else if (scenario !== "offline") {
  const multi = flags.includes("multi");
  state = {
    ...base,
    control: scenario === "connected" ? "local" : (scenario as Snapshot["control"]),
    control_peer: scenario === "remote" || scenario === "controlled" ? "n2" : null,
    connected: { id: "n2", name: "Studio-PC" },
    paired: multi
      ? [
          { id: "n2", name: "Studio-PC", side: "Right", connected: true, nearby: true, color: "#a78bfa", ping_ms: 6, device: dev({ battery: null, plugged: true, cpu: 61, mem: 71, uptime_min: 4300 }) },
          { id: "n3", name: "Laptop", side: "Left", connected: true, nearby: true, color: "#fbbf24", ping_ms: 24, device: dev({ battery: 34, accessories: [{ name: "Galaxy Buds2", battery: 80 }] }) },
          { id: "n4", name: "Office-PC", side: "Top", connected: false, nearby: false, color: "#38bdf8", wakeable: true },
        ]
      : [{ id: "n2", name: "Studio-PC", side: "Right", connected: true, nearby: true, color: "#a78bfa", ping_ms: 9, device: dev({ battery: 34 }) }],
  };
}
if (flags.includes("xfer")) {
  state.transfers = [
    { id: 1, name: "holiday-photos", direction: "out", bytes: 48_000_000, total: 120_000_000, state: "active", message: "", path: null, peer: "Studio-PC" },
    { id: 2, name: "invoice.pdf", direction: "in", bytes: 220_000, total: 220_000, state: "done", message: "", path: "C:\\Users\\me\\Downloads\\CubeCursor", peer: "Studio-PC" },
  ];
}
const listeners: ((s: Snapshot) => void)[] = [];
const push = () => listeners.forEach((l) => l(structuredClone(state)));

const hashHas = (w: string) => typeof location !== "undefined" && location.hash.includes(w);
let polls = 0;
let lic: Record<string, unknown> = hashHas("signedin")
  ? { active: true, kind: "paid", exp: Math.floor(Date.now() / 1000) + 365 * 86400, ends: null, has_key: false, message: null, email: "you@example.com", moved_to: null }
  : hashHas("moved")
    ? { active: false, kind: "paid", exp: null, ends: null, has_key: false, message: null, email: "you@example.com", moved_to: "FRIEND-PC" }
    : hashHas("trial")
      ? { active: true, kind: "trial", exp: null, ends: Math.floor(Date.now() / 1000) + 9 * 86400, has_key: false, message: null, email: null, moved_to: null }
      : { active: false, kind: null, exp: null, ends: null, has_key: false, message: null, email: null, moved_to: null };

export function onSnapshot(cb: (s: Snapshot) => void) {
  listeners.push(cb);
  setTimeout(() => cb(structuredClone(state)), 0);
}

const events = new Map<string, ((p: unknown) => void)[]>();
export function onEvent(name: string, cb: (p: unknown) => void) {
  events.set(name, [...(events.get(name) ?? []), cb]);
}


// Handy for tests / poking at the UI from the dev console.
(window as unknown as Record<string, unknown>).__mock = {
  emit: (name: string, payload?: unknown) => (events.get(name) ?? []).forEach((cb) => cb(payload)),
  set: (patch: Partial<Snapshot>) => {
    state = { ...state, ...patch };
    push();
  },
  state: () => state,
};

let shellMenuOn = false;
let elevated = false;
let elevatedAutostart = false;

export async function call(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  console.log("[mock]", cmd, args);
  switch (cmd) {
    // Licence (preview only): #signedin, #moved or #trial in the address pick a starting state.
    case "license_status":
      return lic;
    case "signin_start":
      polls = 0;
      return { code: "K7QD-M2XA", interval: 1, expires_in: 600 };
    case "signin_poll":
      polls += 1;
      if (polls < 3) return { status: "pending" };
      lic = { ...lic, active: true, kind: "paid", email: "you@example.com", moved_to: null, exp: Math.floor(Date.now() / 1000) + 365 * 86400 };
      return { status: "done", view: lic };
    case "signin_cancel":
    case "signin_open":
      return;
    case "signout":
    case "license_release":
      lic = { active: false, kind: null, exp: null, ends: null, has_key: false, message: null, email: null, moved_to: null };
      return lic;
    case "license_use_here":
      lic = { ...lic, active: true, moved_to: null };
      return lic;
    case "license_trial":
      lic = { ...lic, active: true, kind: "trial", ends: Math.floor(Date.now() / 1000) + 14 * 86400 };
      return lic;
    case "license_activate":
      throw "That licence key wasn't recognised.";
    case "snapshot":
      return structuredClone(state);
    case "set_side": {
      const s = String(args?.side);
      const names: Record<string, string> = { topleft: "TopLeft", topright: "TopRight", bottomleft: "BottomLeft", bottomright: "BottomRight" };
      const name = names[s] ?? s[0].toUpperCase() + s.slice(1);
      state.paired = state.paired.map((p) => (p.id === args?.peerId ? { ...p, side: name as never } : p));
      push();
      return;
    }
    case "autostart_get":
      return true;
    case "elevation_state":
      return { elevated: elevated, autostart: elevatedAutostart };
    case "restart_elevated":
      elevated = true;
      return;
    case "elevated_autostart_set":
      elevatedAutostart = !!args?.on;
      return;
    case "shell_menu_get":
      return shellMenuOn;
    case "shell_menu_set":
      shellMenuOn = !!args?.on;
      return;
    case "ui_get":
      return { enabled: true, hide_fullscreen: true, monitor_cursor: false };
    case "set_game_mode":
      state.game_mode = !!args?.on;
      push();
      return;
    case "set_switch_delay":
      state.switch_delay_ms = Number(args?.ms ?? 0);
      push();
      return;
    case "set_move_apps":
      state.move_apps = !!args?.on;
      push();
      return;
    case "update_status":
      return { current: "0.1.0", version: hashHas("update") ? "0.2.0" : null, notes: "- Second monitor mode\n- Liquid Glass\n- Everything on Mac too" };
    case "update_check":
      return { current: "0.1.0", version: null, notes: "" };
    case "update_install":
      return new Promise((_, no) => setTimeout(() => no("Preview: nothing to install here"), 1500)) as never;
    case "send_feedback":
      return new Promise((r) => setTimeout(() => r(null), 700)) as never;
    case "undo_last":
      state.undo = null;
      push();
      return;
    case "set_monitor_mode":
      state.monitor_mode = !!args?.on;
      push();
      return;
    case "set_drag_files":
      state.drag_files = !!args?.on;
      push();
      return;
    case "set_clipboard_sync":
      state.clipboard_sync = !!args?.on;
      push();
      return;
    case "stage_begin":
      return "mock";
    case "stage_finish":
      state.transfers = [
        ...state.transfers,
        { id: Date.now(), name: String((args?.names as string[])[0] ?? "file"), direction: "out", bytes: 0, total: 5_000_000, state: "active", message: "", path: null, peer: String(args?.peerId ?? "Studio-PC") },
      ];
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
