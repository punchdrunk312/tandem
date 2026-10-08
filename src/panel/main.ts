import "./panel.css";
import { call, onFileDrop, onSnapshot } from "./api";
import type { Side, Snapshot } from "../types";

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
const keys: Record<string, string> = {};

const app = document.getElementById("app")!;
const header = h("header");
const stage = h("section", { class: "card" });
const layout = h("section", { class: "card" });
const files = h("section", { class: "card" });
const settings = h("section", { class: "card" });
const veil = h("div", { id: "veil" });
document.body.append(veil);
app.append(header, stage, layout, files, settings);

function showVeil(on: boolean) {
  if (on) {
    const c = !!snap?.connected;
    veil.className = `show${c ? "" : " off"}`;
    veil.replaceChildren(icon(""), h("div", {}, c ? `Drop to send to ${snap?.connected?.name}` : "Connect to a computer first"), ...(c ? [h("small", {}, "Files and folders")] : []));
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
  const on = s.control !== "offline";
  const live = s.control === "remote" || s.control === "controlled";
  const text =
    s.control === "offline" ? "Not connected"
    : s.control === "remote" ? `Controlling ${s.connected?.name}`
    : s.control === "controlled" ? `${s.connected?.name} is controlling`
    : `Connected to ${s.connected?.name}`;
  update(header, "header", [s.control, s.connected], () => {
    const logo = h("span");
    logo.innerHTML = logoSvg; // static, trusted markup
    return [logo.firstElementChild as Node, h("h1", {}, "Tandem"), h("div", { class: `pill${on ? " on" : ""}${live ? " live" : ""}` }, h("span", { class: "dot" }), h("span", {}, text))];
  });
}

function renderStage(s: Snapshot) {
  update(stage, "stage", [s.control, s.connected, s.pairing_code, s.nearby, s.paired.map((p) => [p.id, p.nearby, p.connected]), s.error], () => {
    if (s.connected) {
      const name = s.connected.name;
      const sub =
        s.control === "remote" ? "Your mouse and keyboard are on that computer now. Move off its edge, or press Scroll Lock, to come back."
        : s.control === "controlled" ? "That computer is using this one. Move to the edge to hand control back."
        : "Push your cursor through the screen edge to cross over. Copy and paste works between both.";
      return [
        h("div", { class: "hero" },
          h("div", { class: "big" }, s.control === "remote" ? "On " : "Connected to ", h("em", {}, name)),
          h("div", { class: "muted" }, sub),
          h("div", { class: "row" },
            h("button", { class: "primary", onclick: () => call("engage") }, `Jump to ${name}`),
            h("button", { class: "danger", onclick: () => call("disconnect") }, "Disconnect"))),
      ];
    }

    const out: Node[] = [
      h("h2", {}, "Pair a computer"),
      h("div", { class: "code-box" },
        h("div", {}, h("div", { class: "muted" }, "This computer's code"), h("div", { class: "code" }, s.pairing_code)),
        h("button", { class: "link", onclick: () => call("new_code") }, "New code")),
      h("p", { class: "muted", style: "margin-top:10px" },
        "On the other computer, open Tandem, find this computer under Nearby and type this code. You only do this once."),
    ];

    const list = h("div", { class: "stack", style: "margin-top:14px" });
    const paired = s.paired.filter((p) => !s.nearby.some((n) => n.id === p.id));
    for (const n of s.nearby) {
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
    if (!s.nearby.length && !paired.length) {
      list.append(h("div", { class: "muted" }, "Looking for computers running Tandem on this network…"));
    }
    out.push(h("h2", { style: "margin-top:18px" }, "Nearby"), list);

    const ip = h("input", { type: "text", placeholder: "192.168.1.42", value: manualIp, oninput: (e: Event) => (manualIp = (e.target as HTMLInputElement).value) });
    const code = h("input", { type: "text", class: "codein", placeholder: "000-000", maxLength: 7, inputMode: "numeric", value: manualCode, oninput: (e: Event) => (manualCode = (e.target as HTMLInputElement).value) });
    out.push(h("details", { style: "margin-top:14px" }, h("summary", {}, "Can't see it? Connect by IP address"),
      h("div", { class: "row" }, ip, code, h("button", { class: "primary", onclick: () => call("connect", { addr: manualIp, port: 24800, code: manualCode }) }, "Pair"))));
    if (s.error) out.push(h("div", { class: "err", role: "alert" }, s.error));
    return out;
  });
}

function renderLayout(s: Snapshot) {
  const peer = s.paired.find((p) => p.connected);
  layout.hidden = !peer;
  if (!peer) return;
  const side = peer.side.toLowerCase() as Side;
  update(layout, "layout", [peer.id, peer.name, side], () => [
    h("h2", {}, "Where is the other computer?"),
    h("div", { class: "grid3" },
      h("div", { class: "cell me" }, "This computer"),
      ...SIDES.map((sd) =>
        sd === side
          ? h("div", { class: `cell other at-${sd}`, title: peer.name }, peer.name)
          : h("button", { class: `cell slot at-${sd}`, onclick: () => call("set_side", { peerId: peer.id, side: sd }) }, `Place ${sd}`))),
    h("p", { class: "muted", style: "margin-top:10px" }, `Push the cursor through the ${side} edge to reach ${peer.name}.`),
  ]);
}

function renderFiles(s: Snapshot) {
  const connected = !!s.connected;
  const peerName = s.connected?.name ?? "the other computer";
  update(files, "files", [connected, peerName, dropOver, s.transfers.map((t) => [t.id, t.direction, t.state, t.name, t.message, t.total, t.path])], () => {
    const out: Node[] = [
      h("h2", {}, "Files"),
      h("div", { class: `drop${dropOver ? " over" : ""}${connected ? "" : " off"}` },
        icon("ico"),
        h("div", {}, connected ? `Drop files or folders here to send them to ${peerName}` : "Connect to a computer to send files"),
        h("div", { class: "row", style: "justify-content:center;margin-top:10px" },
          h("button", { disabled: !connected, onclick: () => call("pick_and_send") }, "Choose files…"),
          h("button", { onclick: () => call("open_downloads") }, "Open received folder"))),
      h("p", { class: "muted", style: "margin-top:8px" }, "Tip: copy files in Explorer here and paste them on the other computer — no dragging needed."),
    ];
    if (s.transfers.length) {
      const list = h("div", { class: "stack", style: "margin-top:14px" });
      for (const t of [...s.transfers].reverse()) {
        const pct = t.total ? Math.min(100, (t.bytes / t.total) * 100) : t.state === "done" ? 100 : 0;
        list.append(h("div", { class: `xfer ${t.state}`, "data-id": `${t.direction}-${t.id}` },
          h("div", { class: "top" }, h("span", {}, t.direction === "out" ? "↑" : "↓"), h("span", { class: "n" }, t.name),
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
  update(settings, "settings", [s.device_name, s.clipboard_sync, autostart, s.paired.map((p) => [p.id, p.name])], () => {
    const name = h("input", { type: "text", value: s.device_name, maxLength: 48, onchange: (e: Event) => call("set_name", { name: (e.target as HTMLInputElement).value }) });
    const out: Node[] = [
      h("h2", {}, "Settings"),
      h("div", { class: "stack" },
        h("label", { class: "stack", style: "gap:4px" }, h("span", { class: "muted" }, "This computer's name (shown to others, applies after restart)"), name),
        h("label", { class: "toggle" }, h("span", {}, "Share clipboard (text and copied files)"),
          h("input", { type: "checkbox", checked: s.clipboard_sync, onchange: (e: Event) => call("set_clipboard_sync", { on: (e.target as HTMLInputElement).checked }) })),
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

function render(s: Snapshot) {
  snap = s;
  renderHeader(s);
  renderStage(s);
  renderLayout(s);
  renderFiles(s);
  renderSettings(s);
}

onSnapshot(render);
onFileDrop(
  (d) => { if (snap?.connected) void d.send().catch((err) => console.error("send failed", err)); },
  (over) => { dropOver = over; showVeil(over); if (snap) renderFiles(snap); },
);
void call<boolean>("autostart_get").then((v) => { autostart = v; if (snap) renderSettings(snap); });
void call<Snapshot>("snapshot").then((s) => { if (s && !snap) render(s); });
