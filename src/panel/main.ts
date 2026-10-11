import "./panel.css";
import { call, onFileDrop, onSnapshot } from "./api";
import type { Side, Snapshot, UiOptions } from "../types";

// ---------------------------------------------------------------- tiny DOM helper
// Everything that comes from the network (computer names, file names, errors)
// goes through textContent, never innerHTML.
type Child = Node | string | null | false | undefined;
function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<Record<string, unknown>> & { class?: string } = {},
  ...kids: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v === undefined || v === false) continue;
    if (k === "class") el.className = String(v);
    else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v as EventListener);
    else if (k in el) (el as unknown as Record<string, unknown>)[k] = v;
    else el.setAttribute(k, String(v));
  }
  for (const c of kids) if (c) el.append(c);
  return el;
}

const logoSvg = `<svg class="logo" viewBox="0 0 64 64" aria-hidden="true"><defs><clipPath id="lt"><rect x="12" y="15.5" width="30" height="30" rx="7"/></clipPath></defs><rect width="64" height="64" rx="15" fill="#0e1014"/><rect x="21.5" y="19" width="30" height="30" rx="7" fill="#fbbf24"/><rect x="12" y="15.5" width="30" height="30" rx="7" fill="#2dd4bf"/><rect x="21.5" y="19" width="30" height="30" rx="7" fill="#e0f7f4" fill-opacity=".85" clip-path="url(#lt)"/></svg>`;

const fmtBytes = (n: number) => {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return `${n.toFixed(n < 10 ? 1 : 0)} ${u[i]}`;
};

const UPLOAD_ICON = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M12 16V4m0 0L7 9m5-5 5 5"/><path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3"/></svg>`;
function icon(cls: string): HTMLElement {
  const w = h("span", { class: cls });
  w.innerHTML = UPLOAD_ICON; // static, trusted markup
  return w;
}

const SIDES: Side[] = ["left", "right", "top", "bottom"];

// ---------------------------------------------------------------- state
let snap: Snapshot | null = null;
const typedCodes = new Map<string, string>(); // per nearby device, survives re-renders
let manualIp = "";
let manualCode = "";
let dropOver = false;
/** Computer picked up in the layout map, waiting for a place to go. */
let held: string | null = null;
/** Where "Choose files" and dropped files go; empty = the computer being controlled, else all. */
let sendTo = "";
/** The "add another computer" section stays open while the list refreshes. */
let addOpen = false;
const keys: Record<string, string> = {};

const app = document.getElementById("app")!;
const header = h("header");
const stage = h("section", { class: "card" });
const layout = h("section", { class: "card" });
const files = h("section", { class: "card" });
const settings = h("section", { class: "card" });
const topbar = h("section", { class: "card" });
const veil = h("div", { id: "veil" });
document.body.append(veil);
app.append(header, stage, layout, files, settings, topbar);

/** Who the next send goes to, in words. */
function sendLabel(s: Snapshot): string {
  const live = s.paired.filter((p) => p.connected);
  if (live.length <= 1) return live[0]?.name ?? "the other computer";
  const one = live.find((p) => p.id === sendTo);
  if (one) return one.name;
  return s.control_peer ? (live.find((p) => p.id === s.control_peer)?.name ?? "the computer in control") : "every connected computer";
}

function showVeil(on: boolean) {
  if (on) {
    const c = !!snap?.connected;
    veil.className = `show${c ? "" : " off"}`;
    veil.replaceChildren(icon(""), h("div", {}, c && snap ? `Drop to send to ${sendLabel(snap)}` : "Connect to a computer first"), ...(c ? [h("small", {}, "Files and folders")] : []));
  } else {
    veil.className = "";
  }
}

/** Re-render a section only when its inputs changed (keeps focus while typing). */
function update(el: HTMLElement, name: string, key: unknown, build: () => Node[]) {
  const k = JSON.stringify(key);
  if (keys[name] === k) return;
  keys[name] = k;
  el.replaceChildren(...build());
}

// ---------------------------------------------------------------- sections
function renderHeader(s: Snapshot) {
  const live = s.paired.filter((p) => p.connected);
  const ctl = s.paired.find((p) => p.id === s.control_peer)?.name ?? s.connected?.name;
  const on = s.control !== "offline";
  const isLive = s.control === "remote" || s.control === "controlled";
  const text =
    s.control === "offline" ? "Not connected"
    : s.control === "remote" ? `Controlling ${ctl}`
    : s.control === "controlled" ? `${ctl} is controlling`
    : live.length > 1 ? `Connected to ${live.length} computers`
    : `Connected to ${s.connected?.name}`;
  update(header, "header", [text, on, isLive], () => {
    const logo = h("span");
    logo.innerHTML = logoSvg; // static, trusted markup
    return [logo.firstElementChild as Node, h("h1", {}, "Tandem"), h("div", { class: `pill${on ? " on" : ""}${isLive ? " live" : ""}` }, h("span", { class: "dot" }), h("span", {}, text))];
  });
}

/** Pairing: this computer's code, who is nearby, manual IP. */
function pairSection(s: Snapshot, heading: string): Node[] {
  const out: Node[] = [
    h("h2", {}, heading),
    h("div", { class: "code-box" },
      h("div", {}, h("div", { class: "muted" }, "This computer's code"), h("div", { class: "code" }, s.pairing_code)),
      h("button", { class: "link", onclick: () => call("new_code") }, "New code")),
    h("p", { class: "muted", style: "margin-top:10px" },
      "On the other computer, open Tandem, find this computer under Nearby and type this code. You only do this once."),
  ];

  const isLive = (id: string) => s.paired.some((p) => p.id === id && p.connected);
  const nearby = s.nearby.filter((n) => !isLive(n.id));
  const list = h("div", { class: "stack", style: "margin-top:14px" });
  const paired = s.paired.filter((p) => !p.connected && !nearby.some((n) => n.id === p.id));
  for (const n of nearby) {
    const row = h("div", { class: "peer" }, h("div", { class: "grow" }, h("div", { class: "name" }, n.name), h("small", {}, `${n.addr}:${n.port}`)));
    if (n.paired) {
      row.append(h("button", { class: "primary", onclick: () => call("connect_known", { id: n.id }) }, "Connect"));
    } else {
      const input = h("input", {
        type: "text", class: "codein", placeholder: "000-000", maxLength: 7, inputMode: "numeric",
        value: typedCodes.get(n.id) ?? "",
        oninput: (e: Event) => typedCodes.set(n.id, (e.target as HTMLInputElement).value),
        onkeydown: (e: KeyboardEvent) => { if (e.key === "Enter") go(); },
      });
      const go = () => call("connect", { addr: n.addr, port: n.port, code: typedCodes.get(n.id) ?? "" });
      row.append(input, h("button", { class: "primary", onclick: go }, "Pair"));
    }
    list.append(row);
  }
  for (const p of paired) {
    list.append(h("div", { class: "peer" }, h("div", { class: "grow" }, h("div", { class: "name" }, p.name), h("small", {}, "paired · not seen on the network")),
      h("button", { onclick: () => call("connect_known", { id: p.id }) }, "Try connect")));
  }
  if (!nearby.length && !paired.length) {
    list.append(h("div", { class: "muted" }, "Looking for computers running Tandem on this network…"));
  }
  out.push(h("h2", { style: "margin-top:18px" }, "Nearby"), list);

  const ip = h("input", { type: "text", placeholder: "192.168.1.42", value: manualIp, oninput: (e: Event) => (manualIp = (e.target as HTMLInputElement).value) });
  const code = h("input", { type: "text", class: "codein", placeholder: "000-000", maxLength: 7, inputMode: "numeric", value: manualCode, oninput: (e: Event) => (manualCode = (e.target as HTMLInputElement).value) });
  out.push(h("details", { style: "margin-top:14px" }, h("summary", {}, "Can't see it? Connect by IP address"),
    h("div", { class: "row" }, ip, code, h("button", { class: "primary", onclick: () => call("connect", { addr: manualIp, port: 24800, code: manualCode }) }, "Pair"))));
  if (s.error) out.push(h("div", { class: "err", role: "alert" }, s.error));
  return out;
}

const MAX_COMPUTERS = 4;

function renderStage(s: Snapshot) {
  const live = s.paired.filter((p) => p.connected);
  update(stage, "stage", [s.control, s.control_peer, live.map((p) => [p.id, p.name, p.side]), s.pairing_code, s.nearby, s.paired.map((p) => [p.id, p.nearby, p.connected]), s.error, addOpen], () => {
    if (!live.length) return pairSection(s, "Pair a computer");

    const ctl = s.paired.find((p) => p.id === s.control_peer)?.name ?? s.connected?.name ?? "";
    const sub =
      s.control === "remote" ? "Your mouse and keyboard are on that computer now. Move off its edge, or press Scroll Lock, to come back."
      : s.control === "controlled" ? "That computer is using this one. Move to the edge to hand control back."
      : live.length > 1 ? "Push your cursor through an edge to jump to that computer. Copy and paste works between all of them."
      : "Push your cursor through the screen edge to cross over. Copy and paste works between both.";
    const rows = live.map((p) =>
      h("div", { class: "peer" },
        h("div", { class: "grow" }, h("div", { class: "name" }, p.name), h("small", {}, `on your ${p.side.toLowerCase()}${p.id === s.control_peer ? " · in control" : ""}`)),
        h("button", { class: "primary", onclick: () => call("engage", { peerId: p.id }) }, "Jump"),
        h("button", { class: "danger", onclick: () => call("disconnect", { peerId: p.id }) }, "Disconnect")));
    const out: Node[] = [
      h("div", { class: "hero" },
        h("div", { class: "big" },
          s.control === "remote" ? "On " : live.length > 1 ? `Connected to ${live.length} computers` : "Connected to ",
          s.control === "remote" || live.length === 1 ? h("em", {}, s.control === "remote" ? ctl : live[0].name) : null),
        h("div", { class: "muted" }, sub)),
      h("div", { class: "stack", style: "margin-top:12px" }, ...rows),
    ];
    if (live.length < MAX_COMPUTERS) {
      const more = h("details", { style: "margin-top:16px", open: addOpen, ontoggle: (e: Event) => { addOpen = (e.target as HTMLDetailsElement).open; } },
        h("summary", {}, "Add another computer"), ...pairSection(s, "Pair a computer"));
      out.push(more);
    } else if (s.error) {
      out.push(h("div", { class: "err", role: "alert" }, s.error));
    }
    return out;
  });
}

function renderLayout(s: Snapshot) {
  layout.hidden = s.paired.length === 0;
  if (!s.paired.length) return;
  if (held && !s.paired.some((p) => p.id === held)) held = null;
  update(layout, "layout", [s.paired.map((p) => [p.id, p.name, p.side, p.connected]), held], () => {
    const heldPeer = s.paired.find((p) => p.id === held);
    const cells = SIDES.map((sd) => {
      const p = s.paired.find((x) => x.side.toLowerCase() === sd);
      if (p) {
        const picked = p.id === held;
        return h("button", {
          class: `cell other at-${sd}${picked ? " held" : ""}${p.connected ? "" : " away"}`,
          title: picked ? "Click again to put it back" : heldPeer ? `Swap with ${heldPeer.name}` : `Pick up ${p.name}`,
          onclick: () => {
            if (!held) held = p.id;
            else if (held === p.id) held = null;
            else { const id = held; held = null; call("set_side", { peerId: id, side: sd }); }
            keys.layout = "";
            renderLayout(snap!);
          },
        }, h("span", {}, p.name), p.connected ? h("i", { class: "on-dot" }) : null);
      }
      return h("button", {
        class: `cell slot at-${sd}`,
        disabled: !held,
        onclick: () => { const id = held; held = null; if (id) call("set_side", { peerId: id, side: sd }); keys.layout = ""; renderLayout(snap!); },
      }, held ? `Put it ${sd}` : "Empty");
    });
    const live = s.paired.filter((p) => p.connected);
    return [
      h("h2", {}, s.paired.length > 1 ? "Where are your computers?" : "Where is the other computer?"),
      h("div", { class: "grid3" }, h("div", { class: "cell me" }, "This computer"), ...cells),
      h("p", { class: "muted", style: "margin-top:10px" },
        heldPeer
          ? `Now click an empty side — or another computer to swap places with ${heldPeer.name}.`
          : "Click a computer to pick it up, then click the side it should be on."),
      ...live.map((p) => h("p", { class: "muted", style: "margin-top:4px" }, `Push the cursor through the ${p.side.toLowerCase()} edge to reach ${p.name}.`)),
    ];
  });
}

function renderFiles(s: Snapshot) {
  const connected = !!s.connected;
  const live = s.paired.filter((p) => p.connected);
  if (sendTo && !live.some((p) => p.id === sendTo)) sendTo = "";
  const peerName = sendLabel(s);
  const many = live.length > 1;
  update(files, "files", [connected, peerName, many, sendTo, dropOver, live.map((p) => [p.id, p.name]), s.transfers.map((t) => [t.id, t.direction, t.state, t.name, t.message, t.total, t.path, t.peer])], () => {
    const target = many
      ? h("label", { class: "stack", style: "gap:4px;margin-bottom:10px" }, h("span", { class: "muted" }, "Send to"),
          (() => {
            const sel = h("select", { onchange: (e: Event) => { sendTo = (e.target as HTMLSelectElement).value; keys.files = ""; renderFiles(snap!); } },
              h("option", { value: "", selected: sendTo === "" }, "The computer I'm controlling — otherwise all"));
            for (const p of live) sel.append(h("option", { value: p.id, selected: sendTo === p.id }, p.name));
            return sel;
          })())
      : null;
    const out: Node[] = [
      h("h2", {}, "Files"),
      ...(target ? [target] : []),
      h("div", { class: `drop${dropOver ? " over" : ""}${connected ? "" : " off"}` },
        icon("ico"),
        h("div", {}, connected ? `Drop files or folders here to send them to ${peerName}` : "Connect to a computer to send files"),
        h("div", { class: "row", style: "justify-content:center;margin-top:10px" },
          h("button", { disabled: !connected, onclick: () => call("pick_and_send", { peerId: sendTo || undefined }) }, "Choose files…"),
          h("button", { onclick: () => call("open_downloads") }, "Open received folder"))),
      h("p", { class: "muted", style: "margin-top:8px" }, "Tip: copy files in Explorer (Ctrl+C) and paste them on the other computer (Ctrl+V) — no dragging needed."),
    ];
    if (s.transfers.length) {
      const list = h("div", { class: "stack", style: "margin-top:14px" });
      for (const t of [...s.transfers].reverse()) {
        const pct = t.total ? Math.min(100, (t.bytes / t.total) * 100) : t.state === "done" ? 100 : 0;
        list.append(h("div", { class: `xfer ${t.state}`, "data-id": `${t.direction}-${t.id}` },
          h("div", { class: "top" }, h("span", {}, t.direction === "out" ? "↑" : "↓"), h("span", { class: "n" }, many ? `${t.name} · ${t.peer}` : t.name),
            h("span", { class: "s" }, t.state === "done" ? fmtBytes(t.total) : t.state === "error" ? "failed" : `${fmtBytes(t.bytes)} / ${fmtBytes(t.total)}`)),
          h("div", { class: "bar" }, h("i", { style: `width:${pct}%` })),
          t.message && h("div", { class: "msg" }, t.message)));
      }
      out.push(list);
    }
    return out;
  });
  // Progress ticks without rebuilding the DOM.
  for (const t of s.transfers) {
    const bar = files.querySelector<HTMLElement>(`[data-id="${t.direction}-${t.id}"] .bar > i`);
    const lbl = files.querySelector<HTMLElement>(`[data-id="${t.direction}-${t.id}"] .s`);
    if (bar && t.total) bar.style.width = `${Math.min(100, (t.bytes / t.total) * 100)}%`;
    if (lbl && t.state === "active") lbl.textContent = `${fmtBytes(t.bytes)} / ${fmtBytes(t.total)}`;
  }
}

let autostart = false;
function renderSettings(s: Snapshot) {
  update(settings, "settings", [s.device_name, s.clipboard_sync, s.game_mode, s.switch_delay_ms, autostart, s.paired.map((p) => [p.id, p.name])], () => {
    const name = h("input", { type: "text", value: s.device_name, maxLength: 48, onchange: (e: Event) => call("set_name", { name: (e.target as HTMLInputElement).value }) });
    const out: Node[] = [
      h("h2", {}, "Settings"),
      h("div", { class: "stack" },
        h("label", { class: "stack", style: "gap:4px" }, h("span", { class: "muted" }, "This computer's name (shown to others, applies after restart)"), name),
        h("label", { class: "toggle" }, h("span", {}, "Share clipboard (text, images, copied files)"),
          h("input", { type: "checkbox", checked: s.clipboard_sync, onchange: (e: Event) => call("set_clipboard_sync", { on: (e.target as HTMLInputElement).checked }) })),
        h("label", { class: "toggle" }, h("span", {}, "Game mode — don't switch computers while a full-screen app is open"),
          h("input", { type: "checkbox", checked: s.game_mode, onchange: (e: Event) => call("set_game_mode", { on: (e.target as HTMLInputElement).checked }) })),
        h("label", { class: "stack", style: "gap:4px" }, h("span", { class: "muted" }, "Hold at the screen edge before switching"),
          (() => {
            const sel = h("select", { onchange: (e: Event) => call("set_switch_delay", { ms: Number((e.target as HTMLSelectElement).value) }) });
            for (const [ms, label] of [[0, "Instant"], [250, "¼ second"], [500, "½ second"], [1000, "1 second"]] as const) {
              sel.append(h("option", { value: String(ms), selected: s.switch_delay_ms === ms }, label));
            }
            return sel;
          })()),
        h("label", { class: "toggle" }, h("span", {}, "Start Tandem with Windows"),
          h("input", { type: "checkbox", checked: autostart, onchange: async (e: Event) => { const on = (e.target as HTMLInputElement).checked; await call("autostart_set", { on }); autostart = on; } })),
        h("p", { class: "muted" }, h("kbd", {}, "Scroll Lock"), " jumps to the other computer — and brings your mouse straight back if anything feels stuck.")),
    ];
    if (s.paired.length) {
      out.push(h("div", { class: "sep" }), h("div", { class: "muted", style: "margin-bottom:6px" }, "Paired computers"));
      for (const p of s.paired) {
        out.push(h("div", { class: "row", style: "padding:4px 0" }, h("span", { class: "grow" }, p.name), h("button", { class: "danger", onclick: () => call("forget", { id: p.id }) }, "Forget")));
      }
    }
    return out;
  });
}

// ---------------------------------------------------------------- top bar settings
// Rust-side options live in ui.json; sound options are the island's own localStorage
// entry (same origin, so the island picks changes up through the `storage` event).
const ISLAND_KEY = "tandem.island";
function islandSettings(): { soundEnabled: boolean; soundVolume: number; autoCloseInterval: number } {
  try {
    return { soundEnabled: true, soundVolume: 0.12, autoCloseInterval: 15, ...JSON.parse(localStorage.getItem(ISLAND_KEY) ?? "{}") };
  } catch {
    return { soundEnabled: true, soundVolume: 0.12, autoCloseInterval: 15 };
  }
}
function saveIsland(patch: Partial<ReturnType<typeof islandSettings>>) {
  try {
    localStorage.setItem(ISLAND_KEY, JSON.stringify({ ...islandSettings(), ...patch }));
  } catch { /* storage unavailable */ }
}

let ui: UiOptions = { enabled: true, hide_fullscreen: true, monitor_cursor: false };
let shellMenu = false;
function renderTopBar() {
  const is = islandSettings();
  update(topbar, "topbar", [ui, is, shellMenu], () => {
    const setUi = (patch: Partial<UiOptions>) => {
      ui = { ...ui, ...patch };
      void call("ui_set", { opts: ui });
    };
    const toggle = (label: string, on: boolean, change: (v: boolean) => void) =>
      h("label", { class: "toggle" }, h("span", {}, label),
        h("input", { type: "checkbox", checked: on, onchange: (e: Event) => change((e.target as HTMLInputElement).checked) }));
    const screenSel = h("select", { onchange: (e: Event) => setUi({ monitor_cursor: (e.target as HTMLSelectElement).value === "cursor" }) },
      h("option", { value: "primary", selected: !ui.monitor_cursor }, "Main display"),
      h("option", { value: "cursor", selected: ui.monitor_cursor }, "Whichever display the cursor is on"));
    const close = h("select", { onchange: (e: Event) => { saveIsland({ autoCloseInterval: Number((e.target as HTMLSelectElement).value) }); renderTopBar(); } });
    for (const [sec, label] of [[5, "5 seconds"], [15, "15 seconds"], [30, "30 seconds"], [60, "1 minute"]] as const) {
      close.append(h("option", { value: String(sec), selected: is.autoCloseInterval === sec }, label));
    }
    return [
      h("h2", {}, "Top bar"),
      h("div", { class: "stack" },
        toggle("Show the top bar", ui.enabled, (v) => setUi({ enabled: v })),
        toggle("Hide it while a full-screen app is open", ui.hide_fullscreen, (v) => setUi({ hide_fullscreen: v })),
        toggle("Sounds", is.soundEnabled, (v) => { saveIsland({ soundEnabled: v }); renderTopBar(); }),
        h("label", { class: "stack", style: "gap:4px" }, h("span", { class: "muted" }, "Show it on"), screenSel),
        h("label", { class: "stack", style: "gap:4px" }, h("span", { class: "muted" }, "Collapse after"), close),
        toggle("“Send with Tandem” in the Explorer right-click menu", shellMenu, (v) => {
          void call("shell_menu_set", { on: v }).then(() => { shellMenu = v; renderTopBar(); }).catch(() => { shellMenu = !v; renderTopBar(); });
        }),
        h("p", { class: "muted" },
          "At rest the bar is a thin line at the top of the screen. ",
          h("kbd", {}, "Ctrl+Alt+T"), " opens or closes the control bar. ",
          h("kbd", {}, "Ctrl+Alt+U"), " opens or closes the small transfer bar."),
        h("p", { class: "muted" },
          "To send a file by drag: pick it up in Explorer or on the desktop, press ",
          h("kbd", {}, "Space"), ", and drop it on the bar. Let go anywhere else and the bar closes again.")),
    ];
  });
}

function render(s: Snapshot) {
  snap = s;
  renderHeader(s);
  renderStage(s);
  renderLayout(s);
  renderFiles(s);
  renderSettings(s);
  renderTopBar();
}

onSnapshot(render);
onFileDrop(
  (d) => { if (snap?.connected) void d.send(sendTo || undefined).catch((err) => console.error("send failed", err)); },
  (over) => { dropOver = over; showVeil(over); if (snap) renderFiles(snap); },
);
void call<UiOptions>("ui_get").then((v) => { if (v) { ui = v; renderTopBar(); } }).catch(() => {});
void call<boolean>("shell_menu_get").then((v) => { shellMenu = !!v; renderTopBar(); }).catch(() => {});
void call<boolean>("autostart_get").then((v) => { autostart = v; if (snap) renderSettings(snap); });
void call<Snapshot>("snapshot").then((s) => { if (s && !snap) render(s); });
