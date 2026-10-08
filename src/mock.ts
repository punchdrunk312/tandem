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
  connected: null,
  paired: [],
  nearby: [],
  transfers: [],
  clipboard_sync: true,
    game_mode: true,
    switch_delay_ms: 0,
  error: null,
};
let state: Snapshot = { ...base };
if (scenario === "pair") {
  state.nearby = [{ id: "n1", name: "LAPTOP-HOME", addr: "192.168.1.42", port: 24800, paired: false }];
} else if (scenario !== "offline") {
  state = {
    ...base,
    control: scenario === "connected" ? "local" : (scenario as Snapshot["control"]),
    connected: { id: "n2", name: "Studio-PC" },
    paired: [{ id: "n2", name: "Studio-PC", side: "Right", connected: true, nearby: true }],
  };
}
if (flags.includes("xfer")) {
  state.transfers = [
    { id: 1, name: "holiday-photos", direction: "out", bytes: 48_000_000, total: 120_000_000, state: "active", message: "", path: null },
    { id: 2, name: "invoice.pdf", direction: "in", bytes: 220_000, total: 220_000, state: "done", message: "", path: "C:\\Users\\me\\Downloads\\Tandem" },
  ];
}
const listeners: ((s: Snapshot) => void)[] = [];
const push = () => listeners.forEach((l) => l(structuredClone(state)));

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
  set: (patch: Partial<Snapshot>) => {
    state = { ...state, ...patch };
    push();
  },
  state: () => state,
};

export async function call(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  console.log("[mock]", cmd, args);
  switch (cmd) {
    case "snapshot":
      return structuredClone(state);
    case "set_side": {
      const s = String(args?.side);
      state.paired = state.paired.map((p) => ({ ...p, side: (s[0].toUpperCase() + s.slice(1)) as never }));
      push();
      return;
    }
    case "autostart_get":
      return true;
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
    case "set_clipboard_sync":
      state.clipboard_sync = !!args?.on;
      push();
      return;
    case "stage_begin":
      return "mock";
    case "stage_finish":
      state.transfers = [
        ...state.transfers,
        { id: Date.now(), name: String((args?.names as string[])[0] ?? "file"), direction: "out", bytes: 0, total: 5_000_000, state: "active", message: "", path: null },
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
