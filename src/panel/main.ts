import "./panel.css";
import { call, onFileDrop, onSnapshot } from "./api";
import type { ElevState, Side, Snapshot, UiOptions } from "../types";

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
const isCorner = (name: string) => name.length > 6;

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
const topbar = h("section", { class: "card" });
const veil = h("div", { id: "veil" });
document.body.append(veil);

const ICONS = {
  computers: "M3 5h13v9H3zM8 18h3M9.5 14v4M19 8v10M16 18h6",
  files: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8zM14 3v5h5M9 13h6M9 17h4",
  settings: "M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6",
  bar: "M3 4h18v5H3zM7 14h10M9 18h6",
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

type PageId = "computers" | "files" | "settings" | "bar";
const PAGES: { id: PageId; label: string; title: string; sub: string; icon: string }[] = [
  { id: "computers", label: "Computers", title: "Computers", sub: "Pair computers and arrange them around this one.", icon: ICONS.computers },
  { id: "files", label: "Files", title: "Files", sub: "Send files and folders to your other computers.", icon: ICONS.files },
  { id: "settings", label: "Settings", title: "Settings", sub: "How CubeCursor behaves on this computer.", icon: ICONS.settings },
  { id: "bar", label: "Top bar", title: "Top bar", sub: "The thin line at the top of your screen.", icon: ICONS.bar },
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
bodyOf("files").append(files);
bodyOf("settings").append(settings, admin, paired);
bodyOf("bar").append(topbar);

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

/** Pairing: this computer's code, who is nearby, manual IP. */
function pairSection(s: Snapshot, heading: string): Node[] {
  const out: Node[] = [
    h("h2", {}, heading),
    h("div", { class: "code-box" },
      h("div", {}, h("div", { class: "muted" }, "This computer's code"), h("div", { class: "code" }, s.pairing_code)),
      h("button", { class: "link", onclick: () => call("new_code") }, "New code")),
    h("p", { class: "muted", style: "margin-top:10px" },
      "On the other computer, open CubeCursor, find this computer under Nearby and type this code. You only do this once."),
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
    list.append(h("div", { class: "muted" }, "Looking for computers running CubeCursor on this network…"));
  }
  out.push(h("h2", { style: "margin-top:18px" }, "Nearby"), list);

  const ip = h("input", { type: "text", placeholder: "192.168.1.42", value: manualIp, oninput: (e: Event) => (manualIp = (e.target as HTMLInputElement).value) });
  const code = h("input", { type: "text", class: "codein", placeholder: "000-000", maxLength: 7, inputMode: "numeric", value: manualCode, oninput: (e: Event) => (manualCode = (e.target as HTMLInputElement).value) });
  out.push(h("details", { style: "margin-top:14px" }, h("summary", {}, "Can't see it? Connect by IP address"),
    h("div", { class: "row" }, ip, code, h("button", { class: "primary", onclick: () => call("connect", { addr: manualIp, port: 24800, code: manualCode }) }, "Pair"))));
  if (s.error) out.push(h("div", { class: "err", role: "alert" }, s.error));
  return out;
}

const MAX_COMPUTERS = 8;

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
        h("div", { class: "grow" }, h("div", { class: "name" }, p.name), h("small", {}, `on your ${sideLabel(p.side)}${p.id === s.control_peer ? " · in control" : ""}`)),
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
      }, held ? `Put it here` : SIDE_LABEL[sd]);
    });
    const live = s.paired.filter((p) => p.connected);
    return [
      h("h2", {}, s.paired.length > 1 ? "Where are your computers?" : "Where is the other computer?"),
      h("div", { class: "grid3" }, h("div", { class: "cell me" }, "This computer"), ...cells),
      h("p", { class: "muted", style: "margin-top:10px" },
        heldPeer
          ? `Now click an empty side — or another computer to swap places with ${heldPeer.name}.`
          : "Click a computer to pick it up, then click where it should sit. Corners work too — push into the corner to reach a diagonal computer."),
      ...live.map((p) => h("p", { class: "muted", style: "margin-top:4px" }, `Push the cursor into the ${sideLabel(p.side)} ${isCorner(p.side) ? "corner" : "edge"} to reach ${p.name}.`)),
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

  // Signed out, or on the free trial.
  out.push(h("div", { class: "lic-card" },
    h("b", {}, L.active ? `Free trial · ${daysLeft(L.ends)} day${daysLeft(L.ends) === 1 ? "" : "s"} left` : "Sign in to use CubeCursor"),
    h("p", {}, "Sign in with the email you bought CubeCursor with. Your other computers don't need to sign in; only this one."),
    h("div", { class: "actions" },
      h("button", { class: "primary", disabled: licenseBusy, onclick: () => void startSignIn() }, licenseBusy ? "Working…" : "Sign in"),
      ...(L.active ? [] : [h("button", { disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("license_trial"), "Your 14-day trial has started.") }, "Start free trial")]),
      h("button", { class: "link", onclick: () => { showKeyEntry = !showKeyEntry; rerenderSettings(); } }, showKeyEntry ? "Hide key" : "Enter a key instead"))));
  if (showKeyEntry) {
    out.push(h("div", { class: "license-entry" },
      h("input", { type: "text", class: "name-in", value: licenseKeyInput, placeholder: "CUBE-XXXX-XXXX-XXXX-XXXX", autocomplete: "off", spellcheck: false, oninput: (e: Event) => { licenseKeyInput = (e.target as HTMLInputElement).value; } }),
      h("button", { class: "primary", disabled: licenseBusy, onclick: () => licenseDo(() => call<LicenseView>("license_activate", { key: licenseKeyInput, takeover: false }), "Licence activated.") }, "Activate"),
      h("button", { disabled: licenseBusy, title: "Move it from the computer that has it now", onclick: () => licenseDo(() => call<LicenseView>("license_activate", { key: licenseKeyInput, takeover: true }), "Licence moved to this computer.") }, "Move here")));
  }
  return [...out, ...err];
}

type WindowsMode = "off" | "move" | "monitor";
const WINDOWS_HELP: Record<WindowsMode, string> = {
  off: "Windows stop at the edge of the screen.",
  move: "Drag a program by its title bar through the edge that leads to the other computer. It hangs across both screens while you drag. Let go more than halfway over and it opens there and closes here (programs can still ask you to save); let go sooner, or press Esc, and it stays. In a browser, drag a tab and hold it at the edge, and the page you are on opens there. Works both ways. It must be turned on at both computers.",
  monitor: "Use the other computer as a second monitor. Drag any window through the edge and let go more than halfway over: it keeps running here and shows there, live. Click and type in it over there like it was on that screen. Drag it back over the edge to bring it home. Works between PCs and Macs.",
};
/** What dragging a window across the edge does: nothing, move the program, or show it there. */
function windowsSetting(s: Snapshot): HTMLElement {
  const mode: WindowsMode = s.monitor_mode ? "monitor" : s.move_apps ? "move" : "off";
  const opts = [["off", "Off"], ["move", "Move the app"], ["monitor", "Second monitor"]] as const;
  // On a Mac, showing another program's window needs Screen Recording.
  const help = WINDOWS_HELP[mode] + (isMac() && mode !== "off" ? " macOS asks once for Screen Recording, so CubeCursor can show the window on the other computer." : "");
  return setting("Drag windows across", help,
    seg<WindowsMode>(opts, mode, (v) => {
      void call("set_monitor_mode", { on: v === "monitor" });
      void call("set_move_apps", { on: v === "move" });
    }));
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
  update(settings, "settings", [s.device_name, s.clipboard_sync, s.drag_files, s.move_apps, s.monitor_mode, s.game_mode, upd, updBusy, updErr, s.switch_delay_ms, autostart, ui.error_reports, plat.os, licenseInfo, licenseKeyInput, licenseBusy, licenseMessage, showKeyEntry, signIn], () => {
    const name = h("input", { type: "text", class: "name-in", value: s.device_name, maxLength: 48, onchange: (e: Event) => call("set_name", { name: (e.target as HTMLInputElement).value }) });
    return [
      h("h2", {}, "General"),
      setting("Computer name", "Shown to your other computers. Applies after a restart.", name),
      setting("Share clipboard", "Text, images and copied files follow you between computers.", switchEl(s.clipboard_sync, (v) => call("set_clipboard_sync", { on: v }))),
      setting("Drag files across", "Pick a file up in Explorer or on the desktop, push it through the edge and let go on the other computer.", switchEl(s.drag_files, (v) => call("set_drag_files", { on: v }))),
      windowsSetting(s),
      setting("Game mode", "Don't switch computers while a full-screen app is open.", switchEl(s.game_mode, (v) => call("set_game_mode", { on: v }))),
      setting("Edge delay", "How long to hold at the screen edge before switching.",
        seg([[0, "Instant"], [250, "¼ s"], [500, "½ s"], [1000, "1 s"]] as const, s.switch_delay_ms as 0 | 250 | 500 | 1000, (ms) => call("set_switch_delay", { ms }))),
      setting(isMac() ? "Start when you log in" : "Start with Windows", null, switchEl(autostart, async (on) => { await call("autostart_set", { on }); autostart = on; })),
      setting("Send error reports", "When something goes wrong, the problem and CubeCursor's own recent log are sent to us so we can fix it. Names, network addresses and folders are removed first. Never what you type, copy or send.",
        switchEl(ui.error_reports === true, (v) => setReports(v))),
      ...licenseSection(),
      updatesRow(),
      h("h2", { style: "margin-top:22px" }, "Feedback"),
      fbCard,
      isMac()
        ? h("div", { class: "note" }, h("span", {}, "Press "), h("kbd", {}, "Esc"), h("span", {}, " three times quickly to bring your mouse and keyboard back if anything feels stuck."))
        : h("div", { class: "note" }, h("kbd", {}, "Scroll Lock"), h("span", {}, " jumps to the other computer and brings your mouse straight back if anything feels stuck.")),
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
}
const ISLAND_DEFAULTS: IslandOpts = {
  soundEnabled: true, soundVolume: 0.12, autoCloseInterval: 15,
  seasons: true, moments: true, crossWhoosh: false, birthday: "", calm: false,
  theme: "glass",
  tips: true,
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

function renderTopBar() {
  const is = islandSettings();
  update(topbar, "topbar", [ui, is, shellMenu, plat], () => {
    const setUi = (patch: Partial<UiOptions>) => {
      ui = { ...ui, ...patch };
      void call("ui_set", { opts: ui });
    };
    return [
      h("h2", {}, "Appearance"),
      setting("Look", isMac()
        ? "Liquid Glass: clear glass you can see your desktop through (macOS asks once for Screen Recording). Classic: the black bar."
        : "Liquid Glass: clear glass you can see your desktop through. To see through itself, the bar stays out of screenshots and screen recordings. Classic: the black bar, which shows up in them.",
        seg([["glass", "Liquid Glass"], ["dark", "Classic"]] as const, is.theme, (v) => { saveIsland({ theme: v }); renderTopBar(); })),
      setting("Show the top bar", "At rest it is a thin line at the top of the screen.", switchEl(ui.enabled, (v) => setUi({ enabled: v }))),
      setting("Hide in full-screen apps", "Keeps games and videos clean.", switchEl(ui.hide_fullscreen, (v) => setUi({ hide_fullscreen: v }))),
      setting("Sounds", null, switchEl(is.soundEnabled, (v) => { saveIsland({ soundEnabled: v }); renderTopBar(); })),
      setting("Show it on", null, seg([["primary", "Main display"], ["cursor", "Cursor's display"]] as const, ui.monitor_cursor ? "cursor" : "primary", (v) => setUi({ monitor_cursor: v === "cursor" }))),
      setting("Collapse after", "How long the bar stays open when you leave it alone.",
        seg([[5, "5 s"], [15, "15 s"], [30, "30 s"], [60, "1 min"]] as const, is.autoCloseInterval as 5 | 15 | 30 | 60, (sec) => { saveIsland({ autoCloseInterval: sec }); renderTopBar(); })),
      h("h2", { style: "margin-top:22px" }, "Bob"),
      setting("Seasons & holidays", "Bob dresses for winter, spring, summer and autumn, and for Christmas, Easter, Halloween and New Year.", switchEl(is.seasons, (v) => { saveIsland({ seasons: v }); renderTopBar(); })),
      setting("Device moments", "A short card when a laptop is plugged in, runs low or is full, or a computer joins or leaves.", switchEl(is.moments, (v) => { saveIsland({ moments: v }); renderTopBar(); })),
      setting("Whoosh when crossing", "A soft sound as the cursor moves to another computer.", switchEl(is.crossWhoosh, (v) => { saveIsland({ crossWhoosh: v }); renderTopBar(); })),
      setting("Daily tips", "Once a day, Bob shows one thing CubeCursor can do.", switchEl(is.tips, (v) => { saveIsland({ tips: v }); renderTopBar(); })),
      setting("Calm Bob", "Nothing drifting in the bar, and quieter moments.", switchEl(is.calm, (v) => { saveIsland({ calm: v }); renderTopBar(); })),
      setting("Your birthday", "Bob celebrates with you. It stays on this computer.", birthdayEl(is.birthday)),
      h("h2", { style: "margin-top:22px" }, "Shortcuts"),
      setting("Control bar", null, h("kbd", {}, isMac() ? "⌃⌥Z" : "Ctrl+Alt+Z")),
      setting("Transfer bar", "Open or close the small transfer bar any time.", h("kbd", {}, isMac() ? "⌃⌥U" : "Ctrl+Alt+U")),
      ...(isMac() ? [] : [setting("Explorer right-click menu", "Adds “Send with CubeCursor” to files and folders.", switchEl(shellMenu, (v) => {
        void call("shell_menu_set", { on: v }).then(() => { shellMenu = v; renderTopBar(); }).catch(() => { shellMenu = !v; renderTopBar(); });
      }))]),
      h("div", { class: "note" }, h("span", {}, "To send by drag: pick a file up, press "), h("kbd", {}, "Space"), h("span", {}, ", and drop it on the bar. Let go anywhere else and the bar closes again.")),
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
void call<boolean>("autostart_get").then((v) => { autostart = v; if (snap) renderSettings(snap); });
void call<Snapshot>("snapshot").then((s) => { if (s && !snap) render(s); });

// ---------------------------------------------------------------- first-launch tour
const TOUR_KEY = "cubecursor.tour.done";
interface TourStep { title: string; text: string; art: () => HTMLElement; focus?: string }
const cursorSvg = `<svg viewBox="0 0 24 24" class="art-cursor"><path d="M5 3l14 8-6 2-2 6z" fill="#12202c" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"/></svg>`;
function artHtml(html: string): HTMLElement {
  const w = h("div", { class: "art" });
  w.innerHTML = html; // static, trusted markup
  return w;
}
const TOUR: TourStep[] = [
  { title: "You're connected!", text: "Nice — your computers are linked. Move your cursor off the edge of one screen and it appears on the other. Here's a 30-second tour of where everything is.",
    art: () => artHtml(`<img class="art-bob" src="/brand.png" alt="">`) },
  { title: "Everything is in this window", text: "Computers lists who is connected and lets you arrange them. Files sends things across. Settings has the switches. Top bar changes the black bar at the top of your screen.",
    focus: ".nav", art: () => artHtml(`<div class="art-desk"><b></b><b class="pulse"></b><b></b><b class="pulse"></b><b class="me"></b><b></b><b></b><b></b><b class="pulse"></b></div>`) },
  { title: "Arrange them like your desk", text: "On the Computers page, click a computer in the layout map, then click where it sits — a side, or a corner for a diagonal. Up to eight computers around this one.",
    focus: ".nav-item[data-page=computers]", art: () => artHtml(`<div class="art-desk"><b></b><b class="pulse"></b><b></b><b class="pulse"></b><b class="me"></b><b></b><b></b><b></b><b class="pulse"></b></div>`) },
  { title: "Drag files across", text: "Pick up a file or folder in Explorer or on the desktop, push it through the edge, and let go on the other computer. It lands in the folder under your cursor. Ctrl+C and Ctrl+V work between computers too.",
    art: () => artHtml(`<div class="art-file"></div>${cursorSvg}`) },
  { title: "Find Settings any time", text: "Open the Settings page for sound, edge delay, game mode and more. Closing this window doesn't quit CubeCursor — it keeps running next to the clock, and clicking its icon there brings this window back.",
    focus: ".nav-item[data-page=settings]", art: () => artHtml(`<div class="art-keys"><kbd>⚙</kbd></div>`) },
  { title: "Handy keys", text: "Ctrl+Alt+Z opens the control bar on the screen your cursor is on. If anything ever feels stuck, press Scroll Lock and your mouse comes straight back. \"Show the tour\" at the bottom left replays this.",
    art: () => artHtml(`<div class="art-keys"><kbd>Ctrl</kbd><kbd>Alt</kbd><kbd>Z</kbd></div>`) },
];
let tourStep = 0;
const tour = h("div", { id: "tour", role: "dialog", "aria-modal": "true", "aria-label": "Welcome tour" });
document.body.append(tour);

function clearFocus() {
  document.querySelectorAll(".tour-focus").forEach((e) => e.classList.remove("tour-focus"));
  tour.classList.remove("aside");
}
function endTour() {
  clearFocus();
  tour.classList.remove("show");
  try { localStorage.setItem(TOUR_KEY, "1"); } catch { /* storage unavailable */ }
}
function renderTour() {
  const st = TOUR[tourStep];
  clearFocus();
  if (st.focus) {
    document.querySelector(st.focus)?.classList.add("tour-focus");
    tour.classList.add("aside");
  }
  const last = tourStep === TOUR.length - 1;
  const art = h("div", { class: "tour-art" });
  art.append(st.art());
  tour.replaceChildren(
    h("div", { class: "tour-card" },
      art,
      h("div", { class: "tour-body tour-step", key: tourStep }, h("h3", {}, st.title), h("p", {}, st.text)),
      h("div", { class: "tour-foot" },
        h("div", { class: "dots" }, ...TOUR.map((_, i) => h("i", { class: i === tourStep ? "on" : "" }))),
        last ? null : h("button", { class: "link", onclick: endTour }, "Skip"),
        tourStep > 0 ? h("button", { onclick: () => { tourStep--; renderTour(); } }, "Back") : null,
        h("button", { class: "primary", onclick: () => { if (last) endTour(); else { tourStep++; renderTour(); } } }, last ? "Get started" : "Next"))));
}
function startTour() {
  tourStep = 0;
  renderTour();
  tour.classList.add("show");
  go("computers");
}
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && tour.classList.contains("show")) endTour(); });
// The tour opens once, right after the first successful connection — not at launch, when
// there is nothing to point at yet. (If storage is unavailable it is simply skipped.)
let tourArmed = true;
function maybeTour(s: Snapshot) {
  if (!tourArmed || !s.connected) return;
  tourArmed = false;
  try {
    if (!localStorage.getItem(TOUR_KEY)) setTimeout(startTour, 1200);
  } catch { /* storage unavailable */ }
}
