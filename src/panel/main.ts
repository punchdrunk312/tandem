import "./panel.css";
import { call, onEvent, onFileDrop, onSnapshot } from "./api";
import type { ElevState, Side, Snapshot, UiOptions } from "../types";
import { RELEASES, markSeen, pending } from "../whatsnew";

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

const logoSvg = `<img class="logo" src="/brand.png" alt="" width="30" height="30">`;

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

const SIDES: Side[] = ["topleft", "top", "topright", "left", "right", "bottomleft", "bottom", "bottomright"];
const SIDE_LABEL: Record<Side, string> = {
  left: "left", right: "right", top: "top", bottom: "bottom",
  topleft: "top-left", topright: "top-right", bottomleft: "bottom-left", bottomright: "bottom-right",
};
const sideLabel = (name: string) => SIDE_LABEL[name.toLowerCase() as Side] ?? name.toLowerCase();

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
const statusEl = h("div", { class: "status" });
const stage = h("section", { class: "card" });
const layout = h("section", { class: "card" });
const files = h("section", { class: "card" });
const settings = h("section", { class: "card" });
const paired = h("section", { class: "card" });
const admin = h("section", { class: "card" });
const topbar = h("div", { class: "feat-page" });
const features = h("div", { class: "feat-page" });
const veil = h("div", { id: "veil" });
document.body.append(veil);

const ICONS = {
  computers: "M3 5h13v9H3zM8 18h3M9.5 14v4M19 8v10M16 18h6",
  files: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8zM14 3v5h5M9 13h6M9 17h4",
  features: "M12 3l2.6 5.6 6.1.7-4.5 4.2 1.2 6L12 16.6 6.6 19.5l1.2-6L3.3 9.3l6.1-.7z",
  bar: "M3 4h18v5H3zM7 14h10M9 18h6",
  account: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0",
  clipboard: "M9 4h6v3H9zM8 5H6a1 1 0 0 0-1 1v14a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V6a1 1 0 0 0-1-1h-2M9 12h6M9 16h4",
  drag: "M7 3h7l4 4v7M7 3a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h4M14 3v4h4M15 15l6 6M21 15v6h-6",
  game: "M6 9h12a4 4 0 0 1 4 4v1a3 3 0 0 1-5.4 1.8L15 14H9l-1.6 1.8A3 3 0 0 1 2 14v-1a4 4 0 0 1 4-4zM7 11v3M5.5 12.5h3M16 12h.01M18 13.5h.01",
  power: "M12 3v9M6.3 6.3a8 8 0 1 0 11.4 0",
  edge: "M4 4v16M10 12h10M16 8l4 4-4 4",
  sound: "M4 9v6h4l5 4V5L8 9zM16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12",
  moments: "M13 2L4.5 13.5H11L10 22l9.5-12H13z",
  seasons: "M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4",
  tips: "M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3z",
  calm: "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z",
  whoosh: "M3 8h11a3 3 0 1 0-3-3M3 16h15a3 3 0 1 1-3 3M3 12h17",
  cake: "M4 21h16v-8H4zM4 16c2 1.5 4 1.5 6 0s4-1.5 6 0 4 1.5 4 0M12 13V8M12 5v.01",
  menu: "M4 6h16M4 12h16M4 18h10",
} as const;
function ico(d: string, size = 18): SVGElement {
  const n = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  n.setAttribute("viewBox", "0 0 24 24");
  n.setAttribute("width", String(size));
  n.setAttribute("height", String(size));
  n.setAttribute("fill", "none");
  n.setAttribute("stroke", "currentColor");
  n.setAttribute("stroke-width", "1.7");
  n.setAttribute("stroke-linecap", "round");
  n.setAttribute("stroke-linejoin", "round");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", d);
  n.append(path);
  return n;
}

type PageId = "computers" | "features" | "files" | "bar" | "account";
const PAGES: { id: PageId; label: string; title: string; sub: string; icon: string }[] = [
  { id: "computers", label: "Computers", title: "Computers", sub: "Pair computers and arrange them around this one.", icon: ICONS.computers },
  { id: "features", label: "Features", title: "Features", sub: "Turn things on and off. Everything is ready to use.", icon: ICONS.features },
  { id: "files", label: "Files", title: "Files", sub: "Send files and folders to your other computers.", icon: ICONS.files },
  { id: "bar", label: "Bar & Bob", title: "Bar & Bob", sub: "How the bar looks and sounds, and what Bob does.", icon: ICONS.bar },
  { id: "account", label: "Account", title: "Account", sub: "Your licence, updates, and getting in touch.", icon: ICONS.account },
];
let page: PageId = "computers";
try {
  const saved = sessionStorage.getItem("tandem.page") as PageId | null;
  if (saved && PAGES.some((x) => x.id === saved)) page = saved;
} catch { /* storage unavailable */ }

const navBadge = new Map<PageId, HTMLElement>();
const indicator = h("i", { class: "nav-ind" });
const navBtns = PAGES.map((pg, i) => {
  const badge = h("span", { class: "badge", hidden: true });
  navBadge.set(pg.id, badge);
  return h("button", { class: "nav-item", "data-page": pg.id, onclick: () => go(pg.id, i) }, ico(pg.icon), h("span", {}, pg.label), badge);
});
const nav = h("nav", { class: "nav" }, indicator, ...navBtns);
const pageEls = new Map<PageId, HTMLElement>();
const content = h("main", { class: "content" });
for (const pg of PAGES) {
  const body = h("div", { class: "page-body" });
  const el = h("div", { class: "page", "data-page": pg.id }, h("div", { class: "page-head" }, h("h1", {}, pg.title), h("p", {}, pg.sub)), body);
  pageEls.set(pg.id, el);
  content.append(el);
  (pg as unknown as { body: HTMLElement }).body = body;
}
const bodyOf = (id: PageId) => (PAGES.find((x) => x.id === id) as unknown as { body: HTMLElement }).body;
bodyOf("computers").append(stage, layout);
bodyOf("features").append(features);
bodyOf("files").append(files);
bodyOf("bar").append(topbar);
bodyOf("account").append(settings, admin, paired);

function go(id: PageId, idx = PAGES.findIndex((x) => x.id === id)) {
  page = id;
  try { sessionStorage.setItem("tandem.page", id); } catch { /* storage unavailable */ }
  indicator.style.transform = `translateY(${idx * 46}px)`;
  navBtns.forEach((b, i) => b.classList.toggle("on", i === idx));
  for (const [pid, el] of pageEls) el.classList.toggle("on", pid === id);
  content.scrollTo({ top: 0 });
}

const side = h("aside", { class: "side" },
  h("div", { class: "brand" }, h("span", { class: "brand-logo" }), h("span", { class: "brand-name" }, "CubeCursor")),
  statusEl, nav,
  h("div", { class: "side-foot" },
    h("div", { class: "foot-row" }, h("kbd", {}, "Ctrl+Alt+Z"), h("span", {}, "control bar")),
    h("button", { class: "link", onclick: () => startTour() }, "Show the tour")));
app.append(side, content);
side.querySelector(".brand-logo")!.innerHTML = logoSvg; // static, trusted markup
go(page);

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
  const sub =
    s.control === "offline" ? (s.paired.length ? "Waiting for your computers" : "Pair a computer to start")
    : s.control === "remote" ? "Your keyboard and mouse are over there"
    : s.control === "controlled" ? "Move to the edge to take back"
    : "Push through an edge to jump";
  update(statusEl, "header", [text, sub, on, isLive], () => [
    h("div", { class: `s-dot${on ? " on" : ""}${isLive ? " live" : ""}` }),
    h("div", { class: "s-text" }, h("div", { class: "s-title" }, text), h("div", { class: "s-sub" }, sub)),
  ]);
  const active = s.transfers.filter((t) => t.state === "active").length;
  const fb = navBadge.get("files")!;
  fb.hidden = active === 0;
  fb.textContent = String(active);
}

/** Pairing: this computer's code, who is nearby, manual IP. A card of its own. */
function pairSection(s: Snapshot, heading: string, first = false): Node[] {
  const isLive = (id: string) => s.paired.some((p) => p.id === id && p.connected);
  const nearby = s.nearby.filter((n) => !isLive(n.id));
  const paired = s.paired.filter((p) => !p.connected && !nearby.some((n) => n.id === p.id));
  const list = h("div", { class: "near-list" });
  for (const n of nearby) {
    const row = h("div", { class: "near" },
      h("span", { class: "near-av" }, Array.from(n.name)[0]?.toUpperCase() ?? "?"),
      h("div", { class: "grow" }, h("div", { class: "name" }, n.name), h("small", {}, n.paired ? "Paired before" : "On this network")));
    if (n.paired) {
      row.append(h("button", { class: "primary", onclick: () => call("connect_known", { id: n.id }) }, "Connect"));
    } else {
      const input = h("input", {
        type: "text", class: "codein", placeholder: "000-000", maxLength: 7, inputMode: "numeric", "aria-label": `Code shown on ${n.name}`,
        value: typedCodes.get(n.id) ?? "",
        oninput: (e: Event) => typedCodes.set(n.id, (e.target as HTMLInputElement).value),
        onkeydown: (e: KeyboardEvent) => { if (e.key === "Enter") go(); },
      });
      const go = () => call("connect", { addr: n.addr, port: n.port, code: typedCodes.get(n.id) ?? "" });
      row.append(h("div", { class: "near-go" }, input, h("button", { class: "primary", onclick: go }, "Pair")));
    }
    list.append(row);
  }
  for (const p of paired) {
    list.append(h("div", { class: "near" },
      h("span", { class: "near-av off" }, Array.from(p.name)[0]?.toUpperCase() ?? "?"),
      h("div", { class: "grow" }, h("div", { class: "name" }, p.name), h("small", {}, "Paired · not seen on the network")),
      h("button", { onclick: () => call("connect_known", { id: p.id }) }, "Try again")));
  }
  if (!nearby.length && !paired.length) {
    list.append(h("div", { class: "near searching" }, h("span", { class: "radar" }), h("div", { class: "grow" },
      h("div", { class: "name" }, "Looking for computers…"), h("small", {}, "Open CubeCursor on the other computer, on the same network."))));
  }
  const ip = h("input", { type: "text", placeholder: "192.168.1.42", value: manualIp, oninput: (e: Event) => (manualIp = (e.target as HTMLInputElement).value) });
  const code = h("input", { type: "text", class: "codein", placeholder: "000-000", maxLength: 7, inputMode: "numeric", value: manualCode, oninput: (e: Event) => (manualCode = (e.target as HTMLInputElement).value) });
  return [
    h("div", { class: `pair${first ? " first" : ""}` },
      h("div", { class: "pair-me" },
        h("h2", {}, heading),
        first ? h("ol", { class: "steps" },
          h("li", {}, "Install CubeCursor on your other computer"),
          h("li", {}, "Find this computer in its list"),
          h("li", {}, "Type this code there")) : null,
        h("div", { class: "code-box" },
          h("div", {}, h("div", { class: "muted" }, "This computer's code"), h("div", { class: "code" }, s.pairing_code)),
          h("button", { class: "link", onclick: () => call("new_code") }, "New code")),
        h("p", { class: "muted", style: "margin-top:8px" }, "You only do this once per computer.")),
      h("div", { class: "pair-near" },
        h("h2", {}, "Nearby"),
        list,
        h("details", { style: "margin-top:10px" }, h("summary", {}, "Can't see it? Connect by IP address"),
          h("div", { class: "row" }, ip, code, h("button", { class: "primary", onclick: () => call("connect", { addr: manualIp, port: 24800, code: manualCode }) }, "Pair"))))),
    s.error ? h("div", { class: "err", role: "alert" }, s.error) : null,
  ].filter(Boolean) as Node[];
}

const MAX_COMPUTERS = 8;

/** A little ring with a number in it (battery, load). */
function miniRing(pct: number, color: string, label: string): HTMLElement {
  const r = 15, c = 2 * Math.PI * r;
  const w = h("span", { class: "mring", title: label });
  w.innerHTML = `<svg viewBox="0 0 36 36" width="36" height="36"><circle cx="18" cy="18" r="${r}" fill="none" stroke="var(--surface-3)" stroke-width="3.5"/><circle cx="18" cy="18" r="${r}" fill="none" stroke="${color}" stroke-width="3.5" stroke-linecap="round" stroke-dasharray="${(c * Math.max(0, Math.min(100, pct))) / 100} ${c}" transform="rotate(-90 18 18)"/></svg>`; // numbers only
  w.append(h("b", {}, String(Math.round(pct))));
  return w;
}

/** One computer: who it is, how it is doing, and what you can do with it. */
function peerCard(s: Snapshot, p: Snapshot["paired"][number]): HTMLElement {
  const color = p.color ?? "#14b8a6";
  const d = p.device ?? null;
  const inControl = p.id === s.control_peer;
  const where = `On your ${sideLabel(p.side)}`;
  const status = !p.connected ? (p.nearby ? "Seen nearby · not connected" : "Not connected")
    : inControl ? (s.control === "remote" ? "You're using it now" : "It's using this computer")
    : `${where}${p.ping_ms != null ? ` · ${p.ping_ms} ms` : ""}`;
  const stats: Node[] = [];
  if (p.connected && d) {
    if (d.battery != null) stats.push(h("div", { class: "stat" }, miniRing(d.battery, d.battery <= 20 && !d.plugged ? "#e8743b" : "#22a559", "Battery"), h("span", {}, d.charging ? "Charging" : d.plugged ? "Plugged in" : "Battery")));
    stats.push(h("div", { class: "stat" }, miniRing(d.cpu, d.cpu > 80 ? "#e8743b" : color, "Processor"), h("span", {}, "Busy")));
    for (const a of (d.accessories ?? []).slice(0, 2)) stats.push(h("div", { class: "stat" }, miniRing(a.battery, a.battery <= 15 ? "#e8743b" : "#2c6fd6", a.name), h("span", { class: "acc" }, a.name)));
  }
  const actions: Node[] = p.connected
    ? [h("button", { class: "primary", onclick: () => call("engage", { peerId: p.id }) }, inControl && s.control === "remote" ? "Come back" : "Jump there"),
       h("button", { onclick: () => call("disconnect", { peerId: p.id }) }, "Disconnect")]
    : [h("button", { class: "primary", onclick: () => call("connect_known", { id: p.id }) }, "Connect"),
       ...(p.wakeable ? [h("button", { onclick: () => call("wake_peer", { peerId: p.id }) }, "Wake it")] : [])];
  return h("div", { class: `pc${p.connected ? " on" : ""}${inControl ? " live" : ""}`, style: `--c:${color}` },
    h("div", { class: "pc-top" },
      h("span", { class: "pc-av" }, Array.from(p.name)[0]?.toUpperCase() ?? "?"),
      h("div", { class: "grow" }, h("div", { class: "pc-name" }, p.name), h("div", { class: "pc-sub" }, h("i", { class: "pc-dot" }), status))),
    stats.length ? h("div", { class: "pc-stats" }, ...stats) : null,
    h("div", { class: "pc-act" }, ...actions));
}

const ACC_ICON: Record<string, string> = {
  earbuds: "M7 4a3 3 0 0 0-3 3v3a3 3 0 0 0 3 3v7M17 4a3 3 0 0 1 3 3v3a3 3 0 0 1-3 3v7",
  headphones: "M3 15v-3a9 9 0 0 1 18 0v3M3 15a2 2 0 0 1 2-2h1v7H5a2 2 0 0 1-2-2zM21 15a2 2 0 0 0-2-2h-1v7h1a2 2 0 0 0 2-2z",
  mouse: "M12 3a6 6 0 0 0-6 6v6a6 6 0 0 0 12 0V9a6 6 0 0 0-6-6zM12 7v4",
  keyboard: "M3 6h18v12H3zM7 10h.01M11 10h.01M15 10h.01M7 14h10",
  gamepad: ICONS.game,
  speaker: "M6 3h12v18H6zM12 15a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zM12 7h.01",
  other: "M7 7l10 10-5 5V2l5 5L7 17",
};

/** Bluetooth accessories on this computer: earbuds show each bud and the case. */
function accCard(list: NonNullable<Snapshot["me"]>["accessories"]): HTMLElement {
  return h("section", { class: "card acc-card" },
    h("h2", {}, "Bluetooth on this computer"),
    h("div", { class: "accs" }, ...list.map((a) => {
      const parts = a.left != null || a.right != null
        ? [["L", a.left], ["R", a.right], ["Case", a.case]].filter(([, v]) => v != null).map(([l, v]) =>
            h("div", { class: "acc-part" }, miniRing(v as number, (v as number) <= 15 ? "#e8743b" : "#22a559", `${l}`), h("span", {}, l as string)))
        : [h("div", { class: "acc-part" }, miniRing(a.battery, a.battery <= 15 ? "#e8743b" : "#22a559", "Battery"))];
      return h("div", { class: "acc" },
        h("span", { class: "t-ico", style: "background:#e1efff;color:#2c6fd6" }, ico(ACC_ICON[a.kind ?? "other"] ?? ACC_ICON.other, 20)),
        h("div", { class: "grow" }, h("div", { class: "pc-name" }, a.name), h("div", { class: "pc-sub" }, "Connected")),
        h("div", { class: "acc-parts" }, ...parts));
    })),
    h("p", { class: "muted", style: "margin-top:10px" }, "Earbuds, headsets, mice and keyboards that report their battery show up here, and in the bar when they connect."));
}

function renderStage(s: Snapshot) {
  const live = s.paired.filter((p) => p.connected);
  update(stage, "stage", [s.control, s.control_peer, s.paired.map((p) => [p.id, p.name, p.side, p.connected, p.nearby, p.ping_ms, p.color, p.wakeable, p.device?.battery, p.device?.charging, p.device?.plugged, p.device ? Math.round(p.device.cpu / 10) : null, p.device?.accessories]), s.me?.accessories, s.pairing_code, s.nearby, s.error, addOpen], () => {
    stage.className = s.paired.length ? "stage" : "card";
    if (!s.paired.length) return pairSection(s, "Add your first computer", true);
    const ctl = s.paired.find((p) => p.id === s.control_peer)?.name ?? "";
    const hero = h("div", { class: "hero2" },
      h("div", { class: "big" },
        s.control === "remote" ? "You're on " : s.control === "controlled" ? "" : live.length ? "Connected to " : "Waiting for ",
        h("em", {}, s.control === "remote" || s.control === "controlled" ? ctl : live.length > 1 ? `${live.length} computers` : live[0]?.name ?? (s.paired.length > 1 ? "your computers" : s.paired[0].name)),
        s.control === "controlled" ? " is using this one" : ""),
      h("div", { class: "muted" },
        s.control === "remote" ? "Your mouse and keyboard are over there. Move back through the edge, or press Scroll Lock, to come back."
        : live.length ? "Push your cursor through an edge to cross. Copy and paste works everywhere." : "They connect by themselves when they're on and on this network."));
    const out: Node[] = [hero, h("div", { class: "pcs" }, ...s.paired.map((p) => peerCard(s, p)))];
    const mine = s.me?.accessories ?? [];
    if (mine.length) out.push(accCard(mine));
    if (s.paired.length < MAX_COMPUTERS) {
      out.push(h("details", { class: "add", open: addOpen, ontoggle: (e: Event) => { addOpen = (e.target as HTMLDetailsElement).open; } },
        h("summary", {}, h("span", { class: "plus" }, "+"), "Add another computer"),
        h("div", { class: "card" }, ...pairSection(s, "Pair a computer"))));
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
  update(layout, "layout", [s.paired.map((p) => [p.id, p.name, p.side, p.connected, p.color]), held, s.device_name], () => {
    const heldPeer = s.paired.find((p) => p.id === held);
    const cells = SIDES.map((sd) => {
      const p = s.paired.find((x) => x.side.toLowerCase() === sd);
      if (p) {
        const picked = p.id === held;
        return h("button", {
          class: `cell other at-${sd}${picked ? " held" : ""}${p.connected ? "" : " away"}`,
          style: `--c:${p.color ?? "#f59e0b"}`,
          title: picked ? "Click again to put it back" : heldPeer ? `Swap with ${heldPeer.name}` : `Pick up ${p.name}`,
          onclick: () => {
            if (!held) held = p.id;
            else if (held === p.id) held = null;
            else { const id = held; held = null; call("set_side", { peerId: id, side: sd }); }
            keys.layout = "";
            renderLayout(snap!);
          },
        }, h("i", { class: "scr" }), h("span", {}, p.name), p.connected ? h("i", { class: "on-dot" }) : null);
      }
      return h("button", {
        class: `cell slot at-${sd}`,
        disabled: !held,
        onclick: () => { const id = held; held = null; if (id) call("set_side", { peerId: id, side: sd }); keys.layout = ""; renderLayout(snap!); },
      }, held ? "Put it here" : "");
    });
    return [
      h("h2", {}, "Your desk"),
      h("p", { class: "muted", style: "margin:-6px 0 14px" }, heldPeer
        ? `Now click an empty spot, or another computer to swap places with ${heldPeer.name}.`
        : "Put each computer where it really is, so the cursor crosses the right edge. Click one to pick it up."),
      h("div", { class: "grid3 desk" }, h("div", { class: "cell me" }, h("i", { class: "scr" }), h("span", {}, s.device_name), h("small", {}, "This computer")), ...cells),
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
      h("p", { class: "muted", style: "margin-top:8px" }, "Tip: drag a file from Explorer or the desktop right through the screen edge, or copy it (Ctrl+C) and paste on the other computer (Ctrl+V)."),
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

/** One setting: title and hint on the left, the control on the right. */
function setting(title: string, hint: string | null, control: Node): HTMLElement {
  return h("div", { class: "set" }, h("div", { class: "set-text" }, h("div", { class: "set-title" }, title), hint ? h("div", { class: "set-hint" }, hint) : null), control);
}
function switchEl(on: boolean, change: (v: boolean) => void): HTMLElement {
  const input = h("input", { type: "checkbox", checked: on, onchange: (e: Event) => change((e.target as HTMLInputElement).checked) });
  return h("label", { class: "switch" }, input, h("i"));
}
function seg<T extends string | number>(opts: readonly (readonly [T, string])[], value: T, change: (v: T) => void): HTMLElement {
  const box = h("div", { class: "seg", role: "radiogroup" });
  for (const [v, label] of opts) {
    box.append(h("button", { class: v === value ? "on" : "", role: "radio", "aria-checked": String(v === value), onclick: () => change(v) }, label));
  }
  return box;
}

let autostart = false;
interface LicenseView {
  active: boolean; kind: string | null; exp: number | null; ends: number | null; has_key: boolean; message: string | null;
  /** Signed in with this email (null when signed out or using a key). */
  email?: string | null;
  /** Another computer took the licence; its name. */
  moved_to?: string | null;
}
interface SignInStart { code: string; interval: number; expires_in: number }
interface SignInPoll { status: "pending" | "denied" | "expired" | "done"; view?: LicenseView; error?: string | null }
let licenseInfo: LicenseView = { active: false, kind: null, exp: null, ends: null, has_key: false, message: null };
let licenseKeyInput = "";
let licenseBusy = false;
let licenseMessage = "";
let showKeyEntry = false;
/** While waiting for the person to click Allow in the browser. */
let signIn: { code: string; until: number } | null = null;
let signInTimer = 0;
const licenseDate = (seconds: number | null) => seconds ? new Date(seconds * 1000).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" }) : "—";
const daysLeft = (seconds: number | null) => seconds ? Math.max(0, Math.ceil((seconds * 1000 - Date.now()) / 864e5)) : 0;
async function refreshLicense() {
  try { licenseInfo = await call<LicenseView>("license_status"); } catch { /* browser preview / older app build */ }
  // Feedback answers go to the email they signed in with, unless they type another.
  if (licenseInfo.email && !fbEmail.value) fbEmail.value = licenseInfo.email;
}
function rerenderSettings() { if (snap) renderSettings(snap); }

/** One licence action: shows "Working…", then the result or the error. */
async function licenseDo(fn: () => Promise<LicenseView>, ok: string) {
  licenseBusy = true; licenseMessage = ""; rerenderSettings();
  try { licenseInfo = await fn(); licenseMessage = ok; } catch (e) { licenseMessage = String(e); }
  finally { licenseBusy = false; rerenderSettings(); }
}

async function startSignIn() {
  licenseBusy = true; licenseMessage = ""; rerenderSettings();
  try {
    const r = await call<SignInStart>("signin_start");
    signIn = { code: r.code, until: Date.now() + r.expires_in * 1000 };
    const tick = async () => {
      if (!signIn) return;
      if (Date.now() > signIn.until) { stopSignIn("That took too long. Choose Sign in to try again."); return; }
      try {
        const p = await call<SignInPoll>("signin_poll");
        if (p.status === "pending") { signInTimer = window.setTimeout(tick, r.interval * 1000); return; }
        if (p.view) licenseInfo = p.view;
        if (p.status === "done" && !p.error) stopSignIn(""); 
        else stopSignIn(p.error || (p.status === "denied" ? "Sign-in was cancelled in the browser." : "That sign-in link expired. Choose Sign in to try again."));
      } catch {
        signInTimer = window.setTimeout(tick, Math.max(3, r.interval) * 1000); // offline for a moment: keep waiting
      }
    };
    signInTimer = window.setTimeout(tick, r.interval * 1000);
  } catch (e) { licenseMessage = String(e); }
  finally { licenseBusy = false; rerenderSettings(); }
}
function stopSignIn(message: string) {
  window.clearTimeout(signInTimer);
  if (signIn) void call("signin_cancel").catch(() => {});
  signIn = null; licenseMessage = message; rerenderSettings();
}

function licenseSection(): Node[] {
  const L = licenseInfo;
  const msg = licenseMessage || L.message || "";
  const out: Node[] = [h("h2", {}, "Licence")];
  const err = msg ? [h("div", { class: "err", role: "status" }, msg)] : [];

  if (signIn) {
    out.push(h("div", { class: "lic-card waiting" },
      h("b", {}, "Finish signing in in your browser"),
      h("p", {}, "Click Allow on the page that opened. Check the code matches:"),
      h("code", { class: "lic-code" }, signIn.code),
      h("div", { class: "actions" },
        h("button", { onclick: () => void call("signin_open").catch(() => {}) }, "Open the page again"),
        h("button", { onclick: () => stopSignIn("") }, "Cancel"))));
    return out;
  }

  if (L.moved_to) {
    out.push(h("div", { class: "lic-card warn" },
      h("b", {}, `Your licence is on ${L.moved_to} now`),
      h("p", {}, L.email ? `Signed in as ${L.email}. Use it here instead? ${L.moved_to} stops using it the next time it's online.` : `Use it here instead? ${L.moved_to} stops using it the next time it's online.`),
      h("div", { class: "actions" },
        h("button", { class: "primary", disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("license_use_here"), "This is your main computer again.") }, licenseBusy ? "Working…" : "Use it here"),
        h("button", { disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("signout"), "Signed out.") }, "Sign out"))));
    return [...out, ...err];
  }

  if (L.active && L.kind !== "trial") {
    out.push(h("div", { class: "lic-card ok" },
      h("b", {}, L.email ? `Signed in as ${L.email}` : "Licence active"),
      h("p", {}, `This is your main computer. It works offline until ${licenseDate(L.exp)} and renews by itself whenever it's online.`),
      h("div", { class: "actions" },
        L.email
          ? h("button", { disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("signout"), "Signed out.") }, "Sign out")
          : h("button", { class: "danger", disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("license_release"), "Licence released from this computer.") }, "Release licence"))));
    return [...out, ...err];
  }

  // Signed out, or on the free trial: sign in with the account the key was redeemed to.
  out.push(h("div", { class: "lic-card" },
    h("b", {}, L.active ? `Free trial · ${daysLeft(L.ends)} day${daysLeft(L.ends) === 1 ? "" : "s"} left` : "Sign in to use CubeCursor"),
    h("p", {}, "Sign in with the CubeCursor account your key is redeemed to (redeem it at cubecursor.shop/dashboard). Only this computer signs in: the computers connected to it don't need to."),
    h("div", { class: "actions" },
      h("button", { class: "primary", disabled: licenseBusy, onclick: () => void startSignIn() }, licenseBusy ? "Working…" : "Sign in"),
      ...(L.active ? [] : [h("button", { disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("license_trial"), "Your 14-day trial has started.") }, "Start free trial")]))));
  return [...out, ...err];
}

// ---------------------------------------------------------------- feature tiles
// Each feature is a tile: a picture, its name, one short line, and its switch. The
// longer explanation stays folded away behind "How it works" for those who want it.

const COLORS = {
  teal: ["#d9f5f1", "#0b8f81"],
  amber: ["#fff0d1", "#b86e00"],
  violet: ["#ece8ff", "#6d4fd8"],
  blue: ["#e1efff", "#2c6fd6"],
  rose: ["#ffe6ec", "#cc3d66"],
  green: ["#dff6e7", "#1d8a4c"],
  slate: ["#e9eef2", "#4b6173"],
} as const;
type Color = keyof typeof COLORS;

/** Which tiles have their "How it works" open (kept across redraws). */
const openMore = new Set<string>();

function tile(o: { id: string; icon: string; color: Color; title: string; line: string; more?: string; control: Node; wide?: boolean; on?: boolean }): HTMLElement {
  const [bg, fg] = COLORS[o.color];
  const badge = h("span", { class: "t-ico", style: `background:${bg};color:${fg}` }, ico(o.icon, 20));
  const more = o.more
    ? h("button", { class: "t-more", "aria-expanded": String(openMore.has(o.id)), onclick: (e: Event) => {
        const t = (e.currentTarget as HTMLElement).closest(".tile")!;
        const open = !openMore.has(o.id);
        if (open) openMore.add(o.id); else openMore.delete(o.id);
        t.classList.toggle("open", open);
        (e.currentTarget as HTMLElement).setAttribute("aria-expanded", String(open));
      } }, "How it works")
    : null;
  return h("div", { class: `tile${o.wide ? " wide" : ""}${openMore.has(o.id) ? " open" : ""}${o.on === false ? " off" : ""}`, "data-tile": o.id },
    h("div", { class: "t-top" }, badge, h("div", { class: "t-ctl" }, o.control)),
    h("div", { class: "t-title" }, o.title),
    h("div", { class: "t-line" }, o.line),
    o.more ? h("div", { class: "t-detail" }, o.more) : null,
    more);
}

function group(title: string, ...kids: Node[]): HTMLElement {
  return h("section", { class: "group" }, h("h2", { class: "g-title" }, title), h("div", { class: "tiles" }, ...kids));
}

type WindowsMode = "off" | "move" | "monitor";
const WINDOWS_OPTS: { id: WindowsMode; title: string; line: string; art: string }[] = [
  { id: "off", title: "Stay put", line: "Windows stop at the edge.", art: `<div class="wa wa-off"><b></b><b></b><i></i></div>` },
  { id: "move", title: "Move the app", line: "It opens over there and closes here.", art: `<div class="wa wa-move"><b></b><b></b><i></i></div>` },
  { id: "monitor", title: "Second monitor", line: "Runs here, shows and works over there.", art: `<div class="wa wa-mon"><b></b><b></b><i></i></div>` },
];
const WINDOWS_HELP: Record<WindowsMode, string> = {
  off: "Windows stop at the edge of the screen, like they always have.",
  move: "Drag a program by its title bar through the edge. Let go more than halfway over and it opens there and closes here; let go sooner, or press Esc, and it stays. In a browser, drag a tab to the edge and the page opens there. Turn it on at both computers.",
  monitor: "Drag any window through the edge and let go more than halfway over: it keeps running here and shows there, live, sized for that screen. Click and type in it over there. Drag it back over the edge, or press “Bring windows back” in the bar, to bring it home.",
};

/** What dragging a window across the edge does: three pictures to pick from. */
function windowsChooser(s: Snapshot): HTMLElement {
  // Second monitor is held back for now: anyone who had it on goes back to normal.
  if (s.monitor_mode) void call("set_monitor_mode", { on: false });
  const mode: WindowsMode = s.move_apps ? "move" : "off";
  const pick = (v: WindowsMode) => {
    void call("set_monitor_mode", { on: v === "monitor" });
    void call("set_move_apps", { on: v === "move" });
  };
  // Second monitor isn't in this release: only "stay put" and "move the app".
  const cards = WINDOWS_OPTS.filter((o) => o.id !== "monitor").map((o) => {
    const b = h("button", { class: `wopt${o.id === mode ? " on" : ""}`, role: "radio", "aria-checked": String(o.id === mode), onclick: () => pick(o.id) },
      h("div", { class: "wopt-art" }),
      h("div", { class: "wopt-title" }, o.title),
      h("div", { class: "wopt-line" }, o.line));
    (b.firstChild as HTMLElement).innerHTML = o.art; // static, trusted markup
    return b;
  });
  const mac = isMac() && mode !== "off" ? " macOS asks once for Screen Recording, so the window can be shown on the other computer." : "";
  return h("section", { class: "group" },
    h("h2", { class: "g-title" }, "Windows across screens"),
    h("div", { class: "wopts two", role: "radiogroup" }, ...cards),
    h("p", { class: "g-help" }, WINDOWS_HELP[mode] + mac));
}

function renderFeatures(s: Snapshot) {
  const isl = islandSettings();
  update(features, "features", [s.clipboard_sync, s.drag_files, s.move_apps, s.monitor_mode, s.game_mode, s.switch_delay_ms, autostart, plat.os, isl.accCards, s.me?.accessories], () => [
    windowsChooser(s),
    group("Everyday",
      tile({ id: "clip", icon: ICONS.clipboard, color: "teal", title: "Shared clipboard", line: "Copy here, paste there.", on: s.clipboard_sync,
        more: "Text, pictures and copied files follow you between computers. Press Ctrl+C on one and Ctrl+V on the other.",
        control: switchEl(s.clipboard_sync, (v) => call("set_clipboard_sync", { on: v })) }),
      tile({ id: "drag", icon: ICONS.drag, color: "blue", title: "Drag files across", line: "Push a file through the edge.", on: s.drag_files,
        more: "Pick a file up in Explorer or on the desktop, push it through the edge and let go on the other computer. It lands in the folder under your cursor.",
        control: switchEl(s.drag_files, (v) => call("set_drag_files", { on: v })) }),
      tile({ id: "game", icon: ICONS.game, color: "violet", title: "Game mode", line: "Never switch in the middle of a game.", on: s.game_mode,
        more: "While a game or any full-screen app is open, the cursor stays on this computer even at the edge.",
        control: switchEl(s.game_mode, (v) => call("set_game_mode", { on: v })) }),
      tile({ id: "buds", icon: ACC_ICON.earbuds, color: "blue", title: "Earbuds' battery", line: "AirPods, Galaxy Buds, headsets, mice.", on: isl.accCards,
        more: (s.me?.accessories?.length
          ? `Connected now: ${s.me.accessories.map((a) => `${a.name} ${a.left != null || a.right != null ? `(L ${a.left ?? "–"}% · R ${a.right ?? "–"}%${a.case != null ? ` · case ${a.case}%` : ""})` : `${a.battery}%`}`).join(", ")}. `
          : "Nothing connected right now. ")
          + "When they connect, the bar shows how charged they are; AirPods show each bud and the case. All of them are listed on the Computers page.",
        control: switchEl(isl.accCards, (v) => { saveIsland({ accCards: v }); if (snap) { keys.features = ""; renderFeatures(snap); } }) }),
      tile({ id: "start", icon: ICONS.power, color: "green", title: isMac() ? "Open at login" : "Start with Windows", line: "Ready the moment you sign in.", on: autostart,
        control: switchEl(autostart, async (on) => { await call("autostart_set", { on }); autostart = on; if (snap) renderFeatures(snap); }) })),
    group("Crossing",
      tile({ id: "delay", icon: ICONS.edge, color: "amber", title: "Edge delay", line: "How long to push at the edge before you cross.", wide: true,
        more: "Instant feels like one big screen. A short delay stops accidental jumps when you reach for a scrollbar or the close button.",
        control: seg([[0, "Instant"], [250, "¼ s"], [500, "½ s"], [1000, "1 s"]] as const, s.switch_delay_ms as 0 | 250 | 500 | 1000, (ms) => call("set_switch_delay", { ms })) })),
  ]);
}

// ---------------------------------------------------------------- updates
interface UpdateView { current: string; version: string | null; notes: string }
let upd: UpdateView | null = null;
let updBusy: string | null = null;
let updErr: string | null = null;
function updatesRow(): HTMLElement {
  const cur = upd?.current ?? "";
  const hint = upd?.version
    ? `Version ${upd.version} is ready (you have ${cur}). ${upd.notes.split("\n").map((l) => l.replace(/^[-*•]\s*/, "")).filter(Boolean).slice(0, 2).join(" · ")}`
    : `You have the latest version${cur ? ` (${cur})` : ""}. CubeCursor checks every few hours.`;
  const btn = upd?.version
    ? h("button", { class: "primary", disabled: !!updBusy, onclick: async () => {
        updBusy = "Downloading…"; updErr = null; if (snap) renderSettings(snap);
        try { await call("update_install"); } catch (e) { updBusy = null; updErr = String(e); if (snap) renderSettings(snap); }
      } }, updBusy ?? "Update now")
    : h("button", { disabled: !!updBusy, onclick: async () => {
        updBusy = "Checking…"; if (snap) renderSettings(snap);
        try { upd = await call<UpdateView>("update_check"); } catch { /* offline */ }
        updBusy = null; if (snap) renderSettings(snap);
      } }, updBusy ?? "Check now");
  return setting("Updates", updErr ? `${hint} ${updErr}` : hint, btn);
}
void call<UpdateView>("update_status").then((u) => { upd = u; if (snap) renderSettings(snap); }).catch(() => {});

// ---------------------------------------------------------------- feedback
// Kept outside the render so typing isn't lost when the page redraws.
const fbText = h("textarea", { class: "fb-text", rows: 4, maxLength: 4000, placeholder: "What's on your mind? A bug, an idea, something you love…" }) as HTMLTextAreaElement;
const fbEmail = h("input", { type: "email", class: "name-in fb-email", placeholder: "Your email, if you'd like an answer", maxLength: 200 }) as HTMLInputElement;
const fbLog = h("input", { type: "checkbox", checked: true }) as HTMLInputElement;
const fbShot = h("input", { type: "checkbox" }) as HTMLInputElement;
const fbStatus = h("span", { class: "fb-status" });
const fbSend = h("button", { class: "primary" }, "Send feedback") as HTMLButtonElement;
fbSend.onclick = async () => {
  if (!fbText.value.trim()) { fbStatus.textContent = "Write a few words first."; return; }
  fbSend.disabled = true;
  fbStatus.textContent = fbShot.checked ? "Taking a screenshot and sending…" : "Sending…";
  try {
    await call("send_feedback", { message: fbText.value, email: fbEmail.value.trim(), log: fbLog.checked, screenshot: fbShot.checked });
    fbText.value = "";
    fbStatus.textContent = "Thanks! It went straight to us.";
  } catch (e) {
    fbStatus.textContent = String(e);
  } finally {
    fbSend.disabled = false;
  }
};
const fbCard = h("div", { class: "fb" },
  fbText,
  fbEmail,
  h("label", { class: "fb-check" }, fbLog, h("span", {}, "Include CubeCursor's recent log (names and addresses removed)")),
  h("label", { class: "fb-check" }, fbShot, h("span", {}, "Include a screenshot of my screen")),
  h("div", { class: "row" }, fbSend, fbStatus));

function renderSettings(s: Snapshot) {
  update(settings, "settings", [s.device_name, upd, updBusy, updErr, ui.error_reports, plat.os, licenseInfo, licenseKeyInput, licenseBusy, licenseMessage, showKeyEntry, signIn], () => {
    const name = h("input", { type: "text", class: "name-in", value: s.device_name, maxLength: 48, onchange: (e: Event) => call("set_name", { name: (e.target as HTMLInputElement).value }) });
    return [
      ...licenseSection(),
      updatesRow(),
      setting("What's new", "The new things in this version, as a short tour.", h("button", { onclick: () => showWhatsNew(true) }, "Show me")),
      h("h2", { style: "margin-top:22px" }, "This computer"),
      setting("Computer name", "Shown to your other computers. Applies after a restart.", name),
      setting("Send error reports", "If something breaks, we get the problem and CubeCursor's own log (no names, addresses or anything you type).",
        switchEl(ui.error_reports === true, (v) => setReports(v))),
      h("h2", { style: "margin-top:22px" }, "Feedback"),
      fbCard,
      isMac()
        ? h("div", { class: "note" }, h("span", {}, "Press "), h("kbd", {}, "Esc"), h("span", {}, " three times quickly to bring your mouse and keyboard back if anything feels stuck."))
        : h("div", { class: "note" }, h("kbd", {}, "Scroll Lock"), h("span", {}, " brings your mouse and keyboard straight back if anything feels stuck.")),
    ];
  });
  paired.hidden = s.paired.length === 0;
  update(paired, "paired", s.paired.map((p) => [p.id, p.name, p.connected]), () => [
    h("h2", {}, "Paired computers"),
    ...s.paired.map((p) =>
      h("div", { class: "peer" },
        h("span", { class: `p-dot${p.connected ? " on" : ""}` }),
        h("div", { class: "grow" }, h("div", { class: "name" }, p.name), h("small", {}, p.connected ? "connected" : "not connected")),
        h("button", { class: "danger", onclick: () => call("forget", { id: p.id }) }, "Forget"))),
  ]);
}

// ---------------------------------------------------------------- administrator mode
let elev: ElevState = { elevated: false, autostart: false };
let elevBusy = "";
let elevError = "";
// ---------------------------------------------------------------- Mac permissions
interface PlatformInfo { os: string; perms_ok: boolean }
let plat: PlatformInfo = { os: "windows", perms_ok: true };
const isMac = () => plat.os === "macos";
function renderMacPerms() {
  update(admin, "admin", [plat], () => {
    const open = (which: string) => h("button", { onclick: () => void call("open_mac_privacy", { which }) }, "Open settings");
    return [
      h("h2", {}, "Mac permissions"),
      ...(plat.perms_ok
        ? [h("div", { class: "note" }, h("span", {}, "CubeCursor is allowed to use the keyboard and mouse. All set."))]
        : [
            h("div", { class: "err", role: "alert" }, "macOS needs your OK before CubeCursor can share this Mac's keyboard and mouse."),
            setting("Accessibility", "Lets CubeCursor move the cursor and type when another computer is in control. Turn CubeCursor on in the list.", open("accessibility")),
            setting("Input Monitoring", "Lets CubeCursor see your keyboard and mouse so they can go to another computer. Turn CubeCursor on in the list.", open("input")),
            h("div", { class: "note" }, h("span", {}, "It starts by itself a few seconds after you allow both. No restart needed.")),
          ]),
    ];
  });
}
function platState() {
  void call<PlatformInfo>("platform_info").then((v) => { if (v) { plat = v; if (snap) render(snap); else renderAdmin(); } }).catch(() => {});
}

function renderAdmin() {
  if (isMac()) return renderMacPerms();
  update(admin, "admin", [elev, elevBusy, elevError], () => {
    const restart = h("button", { class: "primary", disabled: elev.elevated || !!elevBusy, onclick: async () => {
      elevBusy = "Waiting for Windows…";
      elevError = "";
      keys.admin = "";
      renderAdmin();
      try {
        await call("restart_elevated");
        // The app quits on success, so reaching this line means it didn't restart.
      } catch (e) {
        elevError = String(e);
      }
      elevBusy = "";
      elevState();
    } }, elev.elevated ? "Running as administrator" : elevBusy || "Restart as administrator");
    return [
      h("h2", {}, "Admin windows"),
      setting("Control admin windows", "Installers, setup wizards and “Run as administrator” apps ignore the mouse and keyboard unless CubeCursor runs as administrator too. Do this on the computer you control.", restart),
      setting("Always start as administrator", elev.elevated ? "Starts with Windows without asking each time." : "Restart as administrator first.",
        switchEl(elev.autostart, async (on) => {
          try {
            elevError = "";
            await call("elevated_autostart_set", { on });
          } catch (e) {
            elevError = String(e);
          }
          elevState();
        })),
      ...(elevError ? [h("div", { class: "err", role: "alert" }, elevError)] : []),
      h("div", { class: "note" }, h("span", {}, elev.elevated
        ? "In administrator mode, Windows doesn't let you drag files from Explorer onto CubeCursor, and “Send with CubeCursor” won't reach it. Use “Choose files…” or copy and paste instead."
        : "The dimmed “Do you want to allow…” screen can never be controlled by any program. When it appears, CubeCursor hands the mouse and keyboard back so you can answer it on that computer.")),
    ];
  });
}
function elevState() {
  void call<ElevState>("elevation_state").then((v) => { if (v) { elev = v; renderAdmin(); } }).catch(() => {});
}

// ---------------------------------------------------------------- top bar settings
// Rust-side options live in ui.json; sound options are the island's own localStorage
// entry (same origin, so the island picks changes up through the `storage` event).
const ISLAND_KEY = "tandem.island";
interface IslandOpts {
  soundEnabled: boolean; soundVolume: number; autoCloseInterval: number;
  seasons: boolean; moments: boolean; crossWhoosh: boolean; birthday: string; calm: boolean;
  theme: "glass" | "dark";
  tips: boolean;
  accCards: boolean;
}
const ISLAND_DEFAULTS: IslandOpts = {
  soundEnabled: true, soundVolume: 0.12, autoCloseInterval: 15,
  seasons: true, moments: true, crossWhoosh: false, birthday: "", calm: false,
  theme: "glass",
  tips: true,
  accCards: true,
};
function islandSettings(): IslandOpts {
  try {
    return { ...ISLAND_DEFAULTS, ...JSON.parse(localStorage.getItem(ISLAND_KEY) ?? "{}") };
  } catch {
    return { ...ISLAND_DEFAULTS };
  }
}
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/** Bob's birthday picker: a month and a day, saved as "MM-DD" (empty = not set). */
function birthdayEl(v: string): HTMLElement {
  const [m0, d0] = v ? v.split("-").map(Number) : [0, 0];
  const month = h("select", { "aria-label": "Month" }) as HTMLSelectElement;
  const day = h("select", { "aria-label": "Day" }) as HTMLSelectElement;
  month.append(h("option", { value: "0" }, "—"), ...MONTHS.map((n, i) => h("option", { value: String(i + 1) }, n)));
  day.append(h("option", { value: "0" }, "—"), ...Array.from({ length: 31 }, (_, i) => h("option", { value: String(i + 1) }, String(i + 1))));
  month.value = String(m0 || 0);
  day.value = String(d0 || 0);
  const save = () => {
    const m = Number(month.value), d = Number(day.value);
    saveIsland({ birthday: m && d ? `${String(m).padStart(2, "0")}-${String(d).padStart(2, "0")}` : "" });
  };
  month.onchange = save;
  day.onchange = save;
  return h("div", { class: "bday" }, month, day);
}
function saveIsland(patch: Partial<ReturnType<typeof islandSettings>>) {
  try {
    localStorage.setItem(ISLAND_KEY, JSON.stringify({ ...islandSettings(), ...patch }));
  } catch { /* storage unavailable */ }
}

let ui: UiOptions = { enabled: true, hide_fullscreen: true, monitor_cursor: false };
let shellMenu = false;
// ---------------------------------------------------------------- error reports
function setReports(on: boolean) {
  ui = { ...ui, error_reports: on };
  void call("ui_set", { opts: ui });
  reportsAsk.remove();
  if (snap) renderSettings(snap);
}
// Asked once, on the first start after this version.
const reportsAsk = h("div", { class: "reports-ask", role: "dialog", "aria-label": "Error reports" },
  h("div", { class: "card" },
    h("h2", {}, "Help make CubeCursor better"),
    h("p", {}, "When something goes wrong, CubeCursor can send us the problem and its own recent log, so we can fix it. Names, network addresses and folders are removed first, and it never includes what you type, copy or send. You can change this any time in Settings."),
    h("div", { class: "row" },
      h("button", { class: "primary", onclick: () => setReports(true) }, "Send error reports"),
      h("button", { onclick: () => setReports(false) }, "No thanks")),
    h("small", {}, "Details: cubecursor.shop/privacy")));
function maybeAskReports() {
  if (ui.error_reports === undefined || ui.error_reports === null) {
    if (!reportsAsk.isConnected) document.body.appendChild(reportsAsk);
  }
}

const LOOKS = [
  { id: "glass", title: "Liquid Glass", line: "Clear glass. Your desktop shows through." },
  { id: "dark", title: "Classic", line: "The black bar. Shows in screenshots." },
] as const;

function renderTopBar() {
  const is = islandSettings();
  update(topbar, "topbar", [ui, is, shellMenu, plat], () => {
    const setUi = (patch: Partial<UiOptions>) => {
      ui = { ...ui, ...patch };
      void call("ui_set", { opts: ui });
      renderTopBar();
    };
    const set = (patch: Partial<IslandOpts>) => { saveIsland(patch); renderTopBar(); };
    const looks = h("div", { class: "looks", role: "radiogroup" }, ...LOOKS.map((l) =>
      h("button", { class: `look${is.theme === l.id ? " on" : ""}`, role: "radio", "aria-checked": String(is.theme === l.id), onclick: () => set({ theme: l.id }) },
        h("div", { class: `look-art ${l.id}` }, h("i", { class: "lb" }, h("b"), h("s"), h("s"))),
        h("div", { class: "wopt-title" }, l.title),
        h("div", { class: "wopt-line" }, l.line))));
    const volume = h("input", {
      type: "range", class: "vol", min: "0", max: "0.2", step: "0.01", value: String(is.soundVolume), "aria-label": "Volume", disabled: !is.soundEnabled,
      onchange: (e: Event) => saveIsland({ soundVolume: Number((e.target as HTMLInputElement).value) }),
    });
    return [
      h("section", { class: "group" },
        h("h2", { class: "g-title" }, "Look"),
        looks,
        h("p", { class: "g-help" }, isMac() ? "Liquid Glass asks once for Screen Recording, so the bar can see what's behind it." : "To see through itself, the Liquid Glass bar stays out of screenshots and screen recordings.")),
      group("Sound",
        tile({ id: "sound", icon: ICONS.sound, color: "teal", title: "Sounds", line: "Soft, calm sounds. Never a beep.", on: is.soundEnabled,
          more: "A gentle chime when your laptop starts charging or is full, a warm hello when a computer joins, and little noises from Bob. Nothing plays during games or films.",
          control: h("div", { class: "row" }, volume, switchEl(is.soundEnabled, (v) => set({ soundEnabled: v }))) }),
        tile({ id: "whoosh", icon: ICONS.whoosh, color: "blue", title: "Whoosh when crossing", line: "A breath of air as you cross.", on: is.crossWhoosh,
          control: switchEl(is.crossWhoosh, (v) => set({ crossWhoosh: v })) })),
      group("Bob",
        tile({ id: "moments", icon: ICONS.moments, color: "amber", title: "Device moments", line: "Charging, battery low, a computer joins.", on: is.moments,
          more: "A short card in the bar when a laptop is plugged in, runs low or is full, or when a computer joins or leaves. Each one only once in a while.",
          control: switchEl(is.moments, (v) => set({ moments: v })) }),
        tile({ id: "tips", icon: ICONS.tips, color: "violet", title: "Daily tips", line: "One thing CubeCursor can do, once a day.", on: is.tips,
          control: switchEl(is.tips, (v) => set({ tips: v })) }),
        tile({ id: "seasons", icon: ICONS.seasons, color: "rose", title: "Seasons & holidays", line: "Bob dresses for the time of year.", on: is.seasons,
          control: switchEl(is.seasons, (v) => set({ seasons: v })) }),
        tile({ id: "calm", icon: ICONS.calm, color: "slate", title: "Calm Bob", line: "Nothing drifting, quieter cards.", on: is.calm,
          control: switchEl(is.calm, (v) => set({ calm: v })) }),
        tile({ id: "bday", icon: ICONS.cake, color: "rose", title: "Your birthday", line: "Bob celebrates with you. It stays here.", wide: true,
          control: birthdayEl(is.birthday) })),
      h("section", { class: "group" },
        h("h2", { class: "g-title" }, "The bar"),
        h("div", { class: "card list" },
          setting("Show the bar", "At rest it's a thin line at the top of the screen.", switchEl(ui.enabled, (v) => setUi({ enabled: v }))),
          setting("Hide in full-screen apps", "Keeps games and videos clean.", switchEl(ui.hide_fullscreen, (v) => setUi({ hide_fullscreen: v }))),
          setting("Show it on", null, seg([["primary", "Main display"], ["cursor", "Cursor's display"]] as const, ui.monitor_cursor ? "cursor" : "primary", (v) => setUi({ monitor_cursor: v === "cursor" }))),
          setting("Close after", "When you leave it alone.",
            seg([[5, "5 s"], [15, "15 s"], [30, "30 s"], [60, "1 min"]] as const, is.autoCloseInterval as 5 | 15 | 30 | 60, (sec) => set({ autoCloseInterval: sec }))),
          ...(isMac() ? [] : [setting("Explorer right-click menu", "Adds “Send with CubeCursor” to files and folders.", switchEl(shellMenu, (v) => {
            void call("shell_menu_set", { on: v }).then(() => { shellMenu = v; renderTopBar(); }).catch(() => { shellMenu = !v; renderTopBar(); });
          }))]))),
      h("section", { class: "group" },
        h("h2", { class: "g-title" }, "Shortcuts"),
        h("div", { class: "keys" },
          h("div", { class: "key" }, h("kbd", {}, isMac() ? "⌃⌥Z" : "Ctrl+Alt+Z"), h("span", {}, "Open the bar")),
          h("div", { class: "key" }, h("kbd", {}, isMac() ? "⌃⌥U" : "Ctrl+Alt+U"), h("span", {}, "Transfer bar")),
          h("div", { class: "key" }, h("kbd", {}, isMac() ? "Esc ×3" : "Scroll Lock"), h("span", {}, "Get your mouse back")),
          h("div", { class: "key" }, h("kbd", {}, "Space"), h("span", {}, "Holding a file: drop it on the bar")))),
    ];
  });
}

function render(s: Snapshot) {
  snap = s;
  maybeTour(s);
  renderHeader(s);
  renderStage(s);
  renderLayout(s);
  renderFiles(s);
  renderFeatures(s);
  renderSettings(s);
  renderAdmin();
  renderTopBar();
}

onSnapshot(render);
onFileDrop(
  (d) => { if (snap?.connected) void d.send(sendTo || undefined).catch((err) => console.error("send failed", err)); },
  (over) => { dropOver = over; showVeil(over); if (snap) renderFiles(snap); },
);
void refreshLicense().then(() => { if (snap) renderSettings(snap); });
void call<UiOptions>("ui_get").then((v) => { if (v) { ui = v; renderTopBar(); maybeAskReports(); if (snap) renderSettings(snap); } }).catch(() => {});
elevState();
platState();
// While macOS permissions are missing, check again every few seconds.
setInterval(() => { if (isMac() && !plat.perms_ok) platState(); }, 3000);
void call<boolean>("shell_menu_get").then((v) => { shellMenu = !!v; renderTopBar(); }).catch(() => {});
void call<boolean>("autostart_get").then((v) => { autostart = v; if (snap) renderFeatures(snap); });
void call<Snapshot>("snapshot").then((s) => { if (s && !snap) render(s); });

// ---------------------------------------------------------------- first-launch tour
const TOUR_KEY = "cubecursor.tour.done";
/** `wait`: the step moves on by itself once the person really does it (an event from the
 * app, or a change in the snapshot). They can always skip the step. */
interface TourStep {
  title: string; text: string; art: () => HTMLElement; focus?: string;
  wait?: { hint: string; event?: string; until?: (s: Snapshot) => boolean };
}
const cursorSvg = `<svg viewBox="0 0 24 24" class="art-cursor"><path d="M5 3l14 8-6 2-2 6z" fill="#12202c" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"/></svg>`;
function artHtml(html: string): HTMLElement {
  const w = h("div", { class: "art" });
  w.innerHTML = html; // static, trusted markup
  return w;
}
const keysArt = (keys: string[]) => () => artHtml(`<div class="art-keys">${keys.map((k) => `<kbd>${k}</kbd>`).join("")}</div>`);
/** Built when it starts, because only then do we know which computer this is (Mac or PC). */
const buildTour = (): TourStep[] => {
const MAC = isMac();
const KEY_BAR = MAC ? "⌃⌥Z" : "Ctrl+Alt+Z";
const KEY_PILL = MAC ? "⌃⌥U" : "Ctrl+Alt+U";
return [
  { title: "Welcome to CubeCursor", text: "One mouse and keyboard for all your computers. This guide takes about two minutes and you do each step yourself, so the shortcuts stick. You can leave any time and replay it from the bottom left.",
    art: () => artHtml(`<img class="art-bob" src="/brand.png" alt="">`) },
  { title: "Connect your other computer", text: "Install CubeCursor on your other computer too and open it. Keep both on the same Wi-Fi or network. They find each other, and you accept the pairing once on each side.",
    wait: { hint: "Waiting for your other computer… (skip if you'll do it later)", until: (s) => !!s.connected },
    focus: ".nav-item[data-page=computers]", art: () => artHtml(`<div class="art-desk"><b></b><b class="pulse"></b><b></b><b class="pulse"></b><b class="me"></b><b></b><b></b><b></b><b class="pulse"></b></div>`) },
  { title: "Arrange them like your desk", text: "On the Computers page, click a computer in the layout map, then click where it sits on your desk: left, right, above or below. This tells CubeCursor which edge to hand your cursor across.",
    focus: ".nav-item[data-page=computers]", art: () => artHtml(`<div class="art-desk"><b></b><b class="pulse"></b><b></b><b class="pulse"></b><b class="me"></b><b></b><b></b><b></b><b class="pulse"></b></div>`) },
  { title: "Now cross over", text: "Push your mouse off the edge you just set, towards the other computer. Your cursor jumps to its screen — and your keyboard goes with it. Push back the same way to return.",
    wait: { hint: "Move your cursor onto the other computer… (skip if you'll try later)", until: (s) => s.control === "remote" || s.control === "controlled" },
    art: () => artHtml(`${cursorSvg}<div class="art-desk"><b class="me"></b><b class="pulse"></b></div>`) },
  { title: `Try it: press ${KEY_BAR}`, text: "This opens the control bar at the top of the screen your cursor is on. It shows who is connected, and has Lock, Sleep and the other quick actions. Press it again to close it.",
    wait: { hint: `Press ${KEY_BAR} now`, event: "hotkey:bar" }, art: keysArt(MAC ? ["⌃", "⌥", "Z"] : ["Ctrl", "Alt", "Z"]) },
  { title: `Try it: press ${KEY_PILL}`, text: "This opens the transfer bar, where you can see and manage files being sent between your computers. Press it again to put it away.",
    wait: { hint: `Press ${KEY_PILL} now`, event: "hotkey:pill" }, art: keysArt(MAC ? ["⌃", "⌥", "U"] : ["Ctrl", "Alt", "U"]) },
  { title: "Send a file", text: "Pick up a file or folder, push it through the edge, and let go on the other computer. Or pick it up, press Space and drop it on the other computer's name in the bar. Ctrl+C and Ctrl+V work between computers too.",
    wait: { hint: "Drag a file across to the other computer… (skip if you'll try later)", until: (s) => s.transfers.length > 0 },
    art: () => artHtml(`<div class="art-file"></div>${cursorSvg}`) },
  { title: MAC ? "If anything feels stuck: Esc, Esc, Esc" : "If anything feels stuck: Scroll Lock", text: MAC ? "Press Esc three times quickly and your mouse and keyboard come straight back to this computer. You'll rarely need it, but it's good to know it's there." : "Press Scroll Lock and your mouse and keyboard come straight back to this computer. You'll rarely need it, but it's good to know it's there.",
    art: keysArt([MAC ? "Esc ×3" : "Scroll Lock"]) },
  { title: "It lives next to the clock", text: "Closing this window doesn't quit CubeCursor. It keeps running in the tray by the clock, and when your computer starts it goes straight there. Click its icon to bring this window back, and right-click for Quit.",
    art: () => artHtml(`<img class="art-bob" src="/brand.png" alt="">`) },
  { title: "You're all set", text: "Features has a switch for everything CubeCursor can do, and Bar & Bob changes how the bar looks and sounds. Enjoy!",
    focus: ".nav-item[data-page=features]", art: () => artHtml(`<div class="art-mon"><b><i class="win"></i></b><b><i class="win ghost"></i></b></div>`) },
];
};
let tourStep = 0;
/** The steps on show: the welcome tour, or what's new after an update. */
let steps: TourStep[] = [];
let onTourEnd: () => void = () => { try { localStorage.setItem(TOUR_KEY, "1"); } catch { /* storage unavailable */ } };
const tour = h("div", { id: "tour", role: "dialog", "aria-modal": "true", "aria-label": "Welcome tour" });
document.body.append(tour);

function clearFocus() {
  document.querySelectorAll(".tour-focus").forEach((e) => e.classList.remove("tour-focus"));
  tour.classList.remove("aside");
}
function endTour() {
  clearFocus();
  tour.classList.remove("show");
  onTourEnd();
}
let stepDone = false;
let advanceTimer = 0;
function nextStep() {
  window.clearTimeout(advanceTimer);
  if (tourStep >= steps.length - 1) endTour();
  else { tourStep++; stepDone = false; renderTour(); }
}
/** The person did what the step asked: show a tick, then move on. */
function stepCompleted() {
  const st = steps[tourStep];
  if (!tour.classList.contains("show") || !st?.wait || stepDone) return;
  stepDone = true;
  renderTour();
  advanceTimer = window.setTimeout(nextStep, 1100);
}
function renderTour() {
  const st = steps[tourStep];
  clearFocus();
  if (st.focus) {
    document.querySelector(st.focus)?.classList.add("tour-focus");
    tour.classList.add("aside");
  }
  const last = tourStep === steps.length - 1;
  const waiting = !!st.wait && !stepDone;
  const art = h("div", { class: "tour-art" });
  art.append(st.art());
  tour.replaceChildren(
    h("div", { class: "tour-card" },
      art,
      h("div", { class: "tour-body tour-step", key: tourStep }, h("h3", {}, st.title), h("p", {}, st.text),
        st.wait ? h("p", { class: stepDone ? "tour-wait done" : "tour-wait" }, stepDone ? "✓ Nice, that's it!" : st.wait.hint) : null),
      h("div", { class: "tour-foot" },
        h("div", { class: "dots" }, ...steps.map((_, i) => h("i", { class: i === tourStep ? "on" : "" }))),
        last ? null : h("button", { class: "link", onclick: endTour }, "Skip guide"),
        tourStep > 0 ? h("button", { onclick: () => { window.clearTimeout(advanceTimer); tourStep--; stepDone = false; renderTour(); } }, "Back") : null,
        h("button", { class: waiting ? "" : "primary", onclick: nextStep }, last ? "Get started" : waiting ? "Skip this step" : "Next"))));
}
function startTour() {
  steps = buildTour();
  onTourEnd = () => { try { localStorage.setItem(TOUR_KEY, "1"); } catch { /* storage unavailable */ } };
  tourStep = 0;
  stepDone = false;
  renderTour();
  tour.classList.add("show");
  go("computers");
}
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && tour.classList.contains("show")) endTour(); });
// The guide opens by itself the first time CubeCursor is opened, and can be replayed any time.
let tourArmed = true;
function maybeTour(s: Snapshot) {
  if (tourSnap(s) || !tourArmed) return;
  tourArmed = false;
  try {
    if (!localStorage.getItem(TOUR_KEY)) setTimeout(startTour, 600);
  } catch { /* storage unavailable */ }
}
/** Lets a waiting step see the new snapshot. Returns false so maybeTour carries on. */
function tourSnap(s: Snapshot): boolean {
  const w = steps[tourStep]?.wait;
  if (tour.classList.contains("show") && w?.until?.(s)) stepCompleted();
  return false;
}
void onEvent<string>("hotkey", (what) => {
  const w = steps[tourStep]?.wait;
  if (w?.event === `hotkey:${what}`) stepCompleted();
});

// ---------------------------------------------------------------- what's new
// After an update, the new features are shown once, as a short tour like the welcome one.
// "Show me" under Account replays the latest release's.
async function currentVersion(): Promise<string> {
  if (upd?.current) return upd.current;
  try {
    upd = await call<UpdateView>("update_status");
  } catch { /* browser preview */ }
  return upd?.current ?? "";
}

async function showWhatsNew(force: boolean) {
  const v = await currentVersion();
  let things = pending(v);
  if (!things.length && force) things = RELEASES[0]?.things ?? [];
  if (!things.length || tour.classList.contains("show")) return;
  steps = [
    { title: `New in CubeCursor ${v || RELEASES[0].version}`, text: `${things.length} new thing${things.length === 1 ? "" : "s"} to show you. It takes a minute.`,
      art: () => artHtml(`<img class="art-bob" src="/brand.png" alt="">`) },
    ...things.map((t) => ({ title: t.title, text: t.text, art: () => artHtml(t.art) })),
  ];
  onTourEnd = () => { if (v) markSeen(v); };
  tourStep = 0;
  renderTour();
  tour.classList.add("show");
}
// Opened right after an update (or from the bar's "See what's new"): show it.
window.setTimeout(() => {
  try {
    if (!localStorage.getItem(TOUR_KEY)) return; // the welcome guide comes first
  } catch { /* storage unavailable */ }
  void showWhatsNew(false);
}, 900);
// Brought to the front by the bar's "See what's new" while it was already open.
window.addEventListener("focus", () => void showWhatsNew(false));
