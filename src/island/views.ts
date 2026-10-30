// The island's views: header, home (status + actions + transfers), drop, note.
// Everything that comes from the network goes through text nodes.

import { fire } from "../bridge";
import type { Snapshot } from "../types";
import { h, svg } from "./dom";
import { fmtMins, momentText } from "./moments";
import { State } from "./state";

export interface ViewHost {
  el: HTMLElement;
  sync(): void;
}

export interface ViewActions {
  collapse(): void;
  toggleSound(): void;
  blip(): void;
  /** The header's computers button: the device cards, or back. */
  toggleDevices(): void;
  /** The header's timer button: the focus timer, or back. */
  toggleFocus(): void;
  /** Shift-drop menu: prepare the files this way, then send. */
  dropAction(a: "asis" | "zip" | "png" | "jpg"): void;
  /** Shift-drop menu: back out without sending. */
  cancelDrop(): void;
}

const ICON = {
  close: "M6 6l12 12M18 6L6 18",
  sliders: "M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6",
  speaker: "M11 5L6 9H3v6h3l5 4V5zM15.5 8.5a5 5 0 010 7M18 6a8.5 8.5 0 010 12",
  speakerOff: "M11 5L6 9H3v6h3l5 4V5zM16 9l5 6M21 9l-5 6",
  devices: "M3 5h12v9H3zM7 18h4M9 14v4M17 8h4v11h-4z",
  timer: "M12 9v4l2.5 1.5M9.5 2.5h5M12 21a8 8 0 100-16 8 8 0 000 16z",
};

const fmtBytes = (n: number) => {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let i = -1;
  do {
    n /= 1024;
    i++;
  } while (n >= 1024 && i < u.length - 1);
  return `${n.toFixed(n < 10 ? 1 : 0)} ${u[i]}`;
};

const SIDE_GLYPH = { Left: "◀", Right: "▶", Top: "▲", Bottom: "▼" } as const;

/** Re-render `el` only when `key` changes, so clicks never land on a node that was just replaced. */
function keyed(el: HTMLElement, key: string, build: () => (Node | string)[]) {
  if (el.dataset.key === key) return;
  el.dataset.key = key;
  el.replaceChildren(...build());
}

// ------------------------------------------------------------------ header

export function buildHeader(a: ViewActions): ViewHost {
  const dot = h("i", { class: "hd-dot" });
  const title = h("span", { class: "hd-title", text: "CubeCursor" });
  const sound = h("button", { class: "icon-btn", title: "Sound", "aria-label": "Toggle sound", onclick: () => a.toggleSound() });
  const devices = h("button", { class: "icon-btn", title: "Your computers", "aria-label": "Show your computers", onclick: () => { a.blip(); a.toggleDevices(); } }, svg(ICON.devices, 15, { stroke: 1.8 }));
  const settings = h("button", { class: "icon-btn", title: "Pair & settings", "aria-label": "Open settings", onclick: () => { a.blip(); fire("open_main_window"); } }, svg(ICON.sliders, 15, { stroke: 1.8 }));
  const close = h("button", { class: "icon-btn", title: "Close", "aria-label": "Close", onclick: () => a.collapse() }, svg(ICON.close, 15, { stroke: 1.8 }));
  const timer = h("button", { class: "icon-btn", title: "Focus timer", "aria-label": "Focus timer", onclick: () => { a.blip(); a.toggleFocus(); } }, svg(ICON.timer, 15, { stroke: 1.8 }));
  const el = h("div", { id: "header" }, dot, title, h("span", { class: "spacer" }), timer, devices, sound, settings, close);
  return {
    el,
    sync() {
      const live = State.snap?.control && State.snap.control !== "offline";
      dot.className = `hd-dot${live ? " on" : ""}`;
      devices.classList.toggle("on", State.view === "devices");
      timer.classList.toggle("on", State.view === "focus" || !!State.focus);
      title.textContent = State.view === "devices" ? "Your computers" : State.view === "focus" ? "Focus" : State.greeting ?? "CubeCursor";
      sound.replaceChildren(svg(State.settings.soundEnabled ? ICON.speaker : ICON.speakerOff, 15, { stroke: 1.8 }));
    },
  };
}

// ------------------------------------------------------------------ home

export function homeText(s: Snapshot | null): { title: string; sub: string } {
  if (!s) return { title: "Starting…", sub: "" };
  const live = s.paired.filter((p) => p.connected);
  const peer = s.connected?.name ?? "";
  const ctl = s.paired.find((p) => p.id === s.control_peer)?.name ?? peer;
  const side = s.paired.find((p) => p.connected)?.side.toLowerCase() ?? "right";
  switch (s.control) {
    case "local":
      if (live.length > 1) {
        return { title: `Connected to ${live.length} computers`, sub: "Push your cursor through an edge to jump to one. To send a file: pick it up, press Space, drop it on a computer's name." };
      }
      return { title: `Connected to ${peer}`, sub: `Push your cursor through the ${side} edge. To send a file: pick it up, press Space, drop it on me.` };
    case "remote":
      return { title: `Controlling ${ctl}`, sub: "Your mouse and keyboard are over there. Push back through the edge, or press Scroll Lock." };
    case "controlled":
      return { title: `${ctl} is driving`, sub: "Move to the edge to take your mouse back." };
    default: {
      const near = s.nearby.find((n) => n.paired) ?? s.paired.find((p) => p.nearby);
      if (near) return { title: `${near.name} is nearby`, sub: "Connect to share your mouse, keyboard and clipboard." };
      if (s.nearby.length) return { title: "A computer wants a friend", sub: `Open settings and type the code shown on ${s.nearby[0].name}.` };
      return { title: "Not connected", sub: s.paired.length ? "Waiting for your other computer to show up…" : "Run CubeCursor on your other computer, then pair them in settings." };
    }
  }
}

export function buildHome(a: ViewActions): ViewHost {
  const title = h("div", { class: "h-title" });
  const sub = h("div", { class: "h-sub" });
  const actions = h("div", { class: "h-actions" });
  const side = h("div", { class: "side-pick" });
  const xfers = h("div", { class: "xfers" });
  const el = h("div", { class: "view home" }, h("div", { class: "h-text" }, title, sub, actions), side, xfers);

  const btn = (label: string, onclick: () => void, cls = "") => h("button", { class: `btn ${cls}`.trim(), text: label, onclick: () => { a.blip(); onclick(); } });

  return {
    el,
    sync() {
      const s = State.snap;
      const t = homeText(s);
      title.textContent = t.title;
      sub.textContent = t.sub;

      const near = s ? s.nearby.find((n) => n.paired) ?? s.paired.find((p) => p.nearby && !p.connected) : undefined;
      const live = (s?.paired ?? []).filter((p) => p.connected);
      keyed(actions, `${s?.control}|${live.map((p) => p.id + p.name).join()}|${near?.id}|${s?.nearby.length}|${s?.undo}|${s?.windows_away}`, () => {
        if (!s) return [];
        // The last move can be taken back for a couple of minutes.
        const undo = s.undo ? [undoBtn(a, s.undo)] : [];
        // Second-monitor mode: windows shown across can always be called home (one that
        // got lost behind other windows, or a computer that was put away).
        const away = s.windows_away ?? 0;
        const back = away
          ? [btn(away === 1 ? "Bring window back" : `Bring ${away} windows back`, () => fire("bring_windows_back"))]
          : [];
        const extra = [...back, ...undo].slice(0, 1);
        if (live.length > 1) {
          // One button per computer: jump straight to it.
          return [...live.slice(0, extra.length ? 1 : 3).map((p) => btn(`→ ${p.name}`, () => fire("engage", { peerId: p.id }), "primary")), ...extra];
        }
        if (s.connected) {
          return [btn(`Jump to ${s.connected.name}`, () => fire("engage"), "primary"), ...(extra.length ? extra : [btn("Disconnect", () => fire("disconnect"))])];
        }
        const out: Node[] = [];
        if (near) out.push(btn("Connect", () => fire("connect_known", { id: near.id }), "primary"));
        out.push(btn(s.paired.length ? "Settings…" : "Pair a computer…", () => fire("open_main_window"), near ? "" : "primary"));
        return out;
      });

      const peer = live[0];
      keyed(side, live.map((p) => `${p.id}${p.side}${p.name}`).join(), () => {
        if (!peer) return [];
        if (live.length > 1) {
          // Several computers: a read-only map of who sits where.
          const mk = (sd: "Left" | "Right" | "Top" | "Bottom") => {
            const occ = live.find((p) => p.side === sd);
            return h("span", {
              class: `side-btn ro at-${sd.toLowerCase()}${occ ? " on" : ""}`,
              title: occ ? `${occ.name} is on your ${sd.toLowerCase()}` : `Nothing on your ${sd.toLowerCase()}`,
            }, occ ? Array.from(occ.name)[0]?.toUpperCase() ?? "?" : SIDE_GLYPH[sd]);
          };
          return [mk("Top"), mk("Left"), h("span", { class: "side-me", text: "me" }), mk("Right"), mk("Bottom")];
        }
        const mk = (sd: "Left" | "Right" | "Top" | "Bottom") =>
          h("button", {
            class: `side-btn at-${sd.toLowerCase()}${peer.side === sd ? " on" : ""}`,
            title: peer.side === sd ? `${peer.name} is on your ${sd.toLowerCase()}` : `Place ${peer.name} on your ${sd.toLowerCase()}`,
            "aria-label": `Place the other computer on the ${sd.toLowerCase()}`,
            onclick: () => { a.blip(); fire("set_side", { peerId: peer.id, side: sd.toLowerCase() }); },
          }, SIDE_GLYPH[sd]);
        return [mk("Top"), mk("Left"), h("span", { class: "side-me", text: "me" }), mk("Right"), mk("Bottom")];
      });

      // Transfers: newest first, at most three rows.
      const rows = [...(s?.transfers ?? [])].reverse().slice(0, 3);
      const many = live.length > 1;
      keyed(xfers, rows.map((r) => `${r.direction}${r.id}${r.state}${r.name}${r.message}${many}`).join(","), () =>
        rows.map((r) =>
          h("div", { class: `xfer ${r.state}`, "data-id": `${r.direction}${r.id}` },
            h("span", { class: "x-dir", text: r.direction === "out" ? "↑" : "↓" }),
            h("div", { class: "x-body" },
              h("div", { class: "x-top" }, h("span", { class: "x-name", text: many ? `${r.name} · ${r.peer}` : r.name }), h("span", { class: "x-state", text: "" })),
              h("div", { class: "x-bar" }, h("i")),
              r.message ? h("div", { class: "x-msg", text: r.message }) : ""))),
      );
      for (const r of rows) {
        const row = xfers.querySelector<HTMLElement>(`[data-id="${r.direction}${r.id}"]`);
        if (!row) continue;
        const pct = r.state === "done" ? 100 : r.total ? Math.min(100, (r.bytes / r.total) * 100) : 0;
        (row.querySelector(".x-bar > i") as HTMLElement).style.width = `${pct}%`;
        (row.querySelector(".x-state") as HTMLElement).textContent =
          r.state === "done" ? (r.direction === "out" ? "sent" : "received") : r.state === "error" ? "failed" : `${fmtBytes(r.bytes)} / ${fmtBytes(r.total)}`;
      }
    },
  };
}

// ------------------------------------------------------------------ drop / note

export function buildDrop(a: ViewActions): ViewHost {
  const big = h("div", { class: "d-title" });
  const small = h("div", { class: "d-sub" });
  const targets = h("div", { class: "d-targets" });
  const menu = h("div", { class: "d-menu" });
  const el = h("div", { class: "view drop" }, h("div", { class: "d-text" }, big, small), targets, menu);
  const mbtn = (label: string, act: "asis" | "zip" | "png" | "jpg") =>
    h("button", { class: "btn d-act", text: label, onclick: () => { a.blip(); a.dropAction(act); } });
  return {
    el,
    sync() {
      // Shift-drop: ask how to prepare the files, instead of sending at once.
      el.classList.toggle("menu", State.dropMenu);
      if (State.dropMenu) {
        big.textContent = "Send as…";
        small.textContent = "Prepare the files, then they're sent.";
        keyed(menu, "menu", () => [
          mbtn("Send as is", "asis"),
          mbtn("Zip it", "zip"),
          mbtn("As PNG", "png"),
          mbtn("As JPG", "jpg"),
          h("button", { class: "btn d-act ghost", text: "Cancel", onclick: () => { a.blip(); a.cancelDrop(); } }),
        ]);
        keyed(targets, "", () => []);
        return;
      }
      keyed(menu, "", () => []);
      const peers = State.connectedPeers;
      const many = peers.length > 1;
      const peer = State.connected?.name;
      if (many) {
        big.textContent = "Drop on a computer";
        small.textContent = "Files and folders land in its Downloads\\CubeCursor folder.";
      } else {
        big.textContent = peer ? `Drop to send to ${peer}` : "Connect to a computer first";
        small.textContent = peer ? "Files and folders land in its Downloads\\CubeCursor folder." : "Open settings to pair, then drop again.";
      }
      el.classList.toggle("bad", !peer);
      el.classList.toggle("many", many);
      // With several computers every name is its own drop target.
      keyed(targets, many ? peers.map((p) => `${p.id}${p.name}${State.dropTarget === p.id}`).join() : "", () =>
        many
          ? peers.map((p) => h("div", { class: `d-pill${State.dropTarget === p.id ? " over" : ""}`, "data-peer": p.id, text: p.name }))
          : []);
    },
  };
}

/** "↶ Undo", titled with what it undoes. */
function undoBtn(a: { blip(): void }, what: string): HTMLElement {
  return h("button", {
    class: "btn undo",
    title: `Undo: ${what}`,
    text: "↶ Undo",
    onclick: () => { a.blip(); fire("undo_last"); },
  });
}

export function buildNote(): ViewHost {
  const msg = h("div", { class: "n-msg" });
  const act = h("div", { class: "n-act" });
  const el = h("div", { class: "view note" }, msg, act);
  let shown: string | null | undefined;
  return {
    el,
    sync() {
      msg.textContent = State.notice;
      // Right after a move ("Moved Discord to …"): a way to take it back.
      const undo = State.snap?.undo ?? null;
      const fresh = undo && /^(Moved|Opened|Sending|Sent|Carrying)/.test(State.notice) ? undo : null;
      if (fresh !== shown) {
        shown = fresh;
        act.replaceChildren(...(fresh ? [undoBtn({ blip: () => {} }, fresh)] : []));
      }
    },
  };
}

// ------------------------------------------------------------------ device moments

const NS = "http://www.w3.org/2000/svg";
const BOLT = "M13 2L4.5 13.5H11L10 22L19.5 10H13L13 2Z";
const PLUG = "M9 2v6M15 2v6M6 8h12v4a6 6 0 01-12 0V8zM12 18v4";

/** A battery ring: the track, the fill (animated), and an icon or the percentage inside. */
function ring(size: number, stroke: number): { el: SVGSVGElement; set(pct: number, color: string, icon: string | null, label: string): void } {
  const el = document.createElementNS(NS, "svg");
  el.setAttribute("viewBox", `0 0 ${size} ${size}`);
  el.setAttribute("width", String(size));
  el.setAttribute("height", String(size));
  el.setAttribute("class", "ring");
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const mk = (cls: string) => {
    const k = document.createElementNS(NS, "circle");
    k.setAttribute("cx", String(size / 2));
    k.setAttribute("cy", String(size / 2));
    k.setAttribute("r", String(r));
    k.setAttribute("class", cls);
    k.setAttribute("stroke-width", String(stroke));
    return k;
  };
  const track = mk("r-track");
  const fill = mk("r-fill");
  fill.setAttribute("stroke-dasharray", String(c));
  fill.setAttribute("stroke-dashoffset", String(c));
  fill.setAttribute("transform", `rotate(-90 ${size / 2} ${size / 2})`);
  const iconG = document.createElementNS(NS, "path");
  iconG.setAttribute("class", "r-icon");
  const text = document.createElementNS(NS, "text");
  text.setAttribute("x", String(size / 2));
  text.setAttribute("y", String(size / 2));
  text.setAttribute("class", "r-text");
  el.append(track, fill, iconG, text);
  return {
    el,
    set(pct, color, icon, label) {
      el.style.setProperty("--c", color);
      // From empty to the level: the fill sweeps round (CSS transition).
      requestAnimationFrame(() => fill.setAttribute("stroke-dashoffset", String(c * (1 - Math.max(0, Math.min(100, pct)) / 100))));
      if (icon) {
        const k = size / 24 * 0.46;
        iconG.setAttribute("d", icon);
        iconG.setAttribute("transform", `translate(${size / 2 - 12 * k} ${size / 2 - 12 * k}) scale(${k})`);
        iconG.style.display = "";
        text.textContent = "";
      } else {
        iconG.style.display = "none";
        text.textContent = label;
      }
    },
  };
}

export interface MomentActions {
  takeApps(peerId: string): void;
  jump(peerId: string): void;
}

export function buildMoment(a: MomentActions): ViewHost & { show(m: import("./moments").Moment): void } {
  const vis = h("div", { class: "m-vis" });
  const title = h("div", { class: "m-title" });
  const sub = h("div", { class: "m-sub" });
  const acts = h("div", { class: "m-actions" });
  const el = h("div", { class: "view moment" }, vis, h("div", { class: "m-text" }, title, sub, acts));
  return {
    el,
    sync() {},
    show(m) {
      // "on" comes from the bar when it really shows this view (never forced here).
      el.className = `view moment${State.view === "moment" ? " on" : ""} k-${m.kind} from-${(m.side ?? "none").toLowerCase()}`;
      el.style.setProperty("--c", m.color);
      // Restart the entrance animation.
      void el.offsetWidth;
      el.classList.add("enter");
      const t = momentText(m);
      title.textContent = t.title;
      sub.textContent = t.sub;
      const d = m.device;
      vis.replaceChildren();
      vis.classList.toggle("parts", false);
      const acc = m.acc;
      if (m.kind === "online" || m.kind === "offline") {
        vis.append(h("div", { class: "m-avatar", text: Array.from(m.name)[0]?.toUpperCase() ?? "?" }));
      } else if (acc && (acc.left != null || acc.right != null)) {
        // Earbuds: each bud and the case, like a phone shows them.
        vis.classList.add("parts");
        const part = (v: number | null | undefined, label: string) => {
          if (v == null) return null;
          const r = ring(46, 5);
          r.set(v, v <= 15 ? "#ff8a5c" : "#34d399", null, `${v}`);
          return h("div", { class: "m-part" }, r.el, h("span", { text: label }));
        };
        vis.append(...[part(acc.left, "L"), part(acc.right, "R"), part(acc.case, "Case")].filter((x): x is HTMLDivElement => !!x));
      } else {
        const r = ring(68, 6);
        vis.append(r.el);
        const pct = m.kind === "accessory" || m.kind === "acc-on" ? m.acc?.battery ?? 0 : d?.battery ?? 0;
        const color = m.kind === "low" || m.kind === "accessory" ? "#ff8a5c" : m.kind === "full" ? "#34d399" : m.color;
        const icon = m.kind === "plug" ? BOLT : m.kind === "unplug" ? PLUG : null;
        r.set(pct, color, icon, `${pct}%`);
      }
      acts.replaceChildren();
      if (m.kind === "low" && m.id !== "me") {
        acts.append(
          h("button", { class: "btn primary", text: "Bring its apps here", onclick: () => a.takeApps(m.id) }),
          h("button", { class: "btn", text: "Jump there", onclick: () => a.jump(m.id) }),
        );
      }
    },
  };
}

// ------------------------------------------------------------------ device cards

export interface DeviceActions {
  jump(peerId: string): void;
  power(peerId: string, action: "lock" | "sleep"): void;
  wake(peerId: string): void;
  blip(): void;
}

const fmtUp = (m: number) => (m < 60 ? `${m} min` : m < 1440 ? `${Math.floor(m / 60)} h` : `${Math.floor(m / 1440)} d`);

export function buildDevices(a: DeviceActions): ViewHost {
  const row = h("div", { class: "dev-row" });
  const el = h("div", { class: "view devices" }, row);
  const btn = (label: string, f: () => void, cls = "") => h("button", { class: `btn mini ${cls}`.trim(), text: label, onclick: () => { a.blip(); f(); } });
  return {
    el,
    sync() {
      const s = State.snap;
      if (!s) return;
      type Card = { id: string; name: string; color: string; d: import("../types").DeviceInfo | null; ping: number | null; me: boolean; connected: boolean; wakeable: boolean };
      const cards: Card[] = [
        { id: "me", name: s.device_name, color: s.my_color ?? "#2dd4bf", d: s.me ?? null, ping: null, me: true, connected: true, wakeable: false },
        ...s.paired
          .filter((p) => p.connected || p.wakeable)
          .map((p) => ({ id: p.id, name: p.name, color: p.color ?? "#2dd4bf", d: p.device ?? null, ping: p.ping_ms ?? null, me: false, connected: p.connected, wakeable: !!p.wakeable })),
      ];
      const key = JSON.stringify(cards);
      keyed(row, key, () =>
        cards.map((c) => {
          const d = c.d;
          const r = ring(46, 4.5);
          const charging = !!d?.charging || (!!d?.plugged && d?.battery != null && d.battery < 100);
          if (d?.battery != null) {
            r.set(d.battery, d.battery <= 20 && !d.plugged ? "#ff8a5c" : c.color, charging ? BOLT : null, `${d.battery}`);
          } else {
            r.set(100, c.color, PLUG, "");
          }
          const line = !c.connected
            ? "Asleep or away"
            : d?.battery == null
              ? "Plugged in"
              : d.plugged
                ? d.battery >= 100 ? "Full" : d.mins_to_full != null ? `Full in ${fmtMins(d.mins_to_full)}` : "Charging"
                : d.mins_left != null ? `${fmtMins(d.mins_left)} left` : "On battery";
          const bar = (label: string, v: number) =>
            h("div", { class: "dv-bar" }, h("span", { text: label }), h("i", {}, h("b", { style: `width:${Math.max(3, Math.min(100, v))}%` })));
          const quality = c.ping == null ? null : c.ping < 30 ? "good" : c.ping < 100 ? "ok" : "bad";
          const acc = (d?.accessories ?? []).slice(0, 2);
          return h("div", { class: `dev-card${c.connected ? "" : " away"}`, style: `--c:${c.color}` },
            h("div", { class: "dv-top" },
              r.el,
              h("div", { class: "dv-name" },
                h("b", { text: c.name }),
                h("span", { text: (c.me ? "This PC · " : "") + line + (d?.saver ? " · saver" : "") }))),
            c.connected && d
              ? h("div", { class: "dv-stats" },
                  bar("CPU", d.cpu),
                  bar("RAM", d.mem),
                  h("div", { class: "dv-meta" },
                    h("span", { text: `${d.disk_free_gb} GB free` }),
                    h("span", { text: `up ${fmtUp(d.uptime_min)}` }),
                    quality ? h("span", { class: `q ${quality}`, text: `${c.ping} ms` }) : null),
                  acc.length ? h("div", { class: "dv-acc" }, ...acc.map((x) => h("span", {
                    class: x.battery <= 15 ? "low" : "",
                    title: x.name,
                    text: x.left != null || x.right != null
                      ? `${x.name} · L ${x.left ?? "–"}% R ${x.right ?? "–"}%${x.case != null ? ` · case ${x.case}%` : ""}`
                      : `${x.name} ${x.battery}%`,
                  }))) : null)
              : h("div", { class: "dv-stats" }),
            c.me
              ? null
              : h("div", { class: "dv-actions" },
                  ...(c.connected
                    ? [btn("Jump", () => a.jump(c.id), "primary"), btn("Lock", () => a.power(c.id, "lock")), btn("Sleep", () => a.power(c.id, "sleep"))]
                    : [btn("Wake", () => a.wake(c.id), "primary")])));
        }),
      );
    },
  };
}


// ------------------------------------------------------------------ focus timer

export interface FocusActions {
  blip(): void;
  start(minutes: number): void;
  stop(): void;
  more(minutes: number): void;
}

const pad2 = (n: number) => String(n).padStart(2, "0");

/** "18:42" for what is left. */
export function clock(ms: number): string {
  const s = Math.max(0, Math.ceil(ms / 1000));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return h ? `${h}:${pad2(m)}:${pad2(r)}` : `${pad2(m)}:${pad2(r)}`;
}

/** Pick a length and start; while it runs, the time left and a way to stop. */
export function buildFocus(a: FocusActions): ViewHost {
  const big = h("div", { class: "f-clock" });
  const sub = h("div", { class: "f-sub" });
  const row = h("div", { class: "f-row" });
  const el = h("div", { class: "view focus" }, h("div", { class: "f-text" }, big, sub, row));
  const btn = (label: string, onclick: () => void, cls = "") => h("button", { class: `btn ${cls}`.trim(), text: label, onclick: () => { a.blip(); onclick(); } });
  let shown = "";
  return {
    el,
    sync() {
      const f = State.focus;
      if (f) {
        const left = f.endsAt - Date.now();
        big.textContent = clock(left);
        const end = new Date(f.endsAt);
        sub.textContent = `Ends at ${pad2(end.getHours())}:${pad2(end.getMinutes())}. Bob keeps the cards quiet until then.`;
        if (shown !== "run") {
          shown = "run";
          row.replaceChildren(btn("+5 min", () => a.more(5)), btn("Stop", () => a.stop()));
        }
      } else {
        const m = State.settings.focusMinutes;
        big.textContent = clock(m * 60_000);
        sub.textContent = "Bob keeps the time and the cards quiet, and cheers when you're done.";
        const key = `pick${m}`;
        if (shown !== key) {
          shown = key;
          const chip = (n: number) => btn(`${n} min`, () => { State.settings.focusMinutes = n; State.saveSettings(); }, n === m ? "on" : "");
          row.replaceChildren(chip(15), chip(25), chip(50), btn("Start", () => a.start(State.settings.focusMinutes), "primary"));
        }
      }
    },
  };
}


// ------------------------------------------------------------------ a new version

export interface UpdateInfo {
  current: string;
  version: string | null;
  notes: string;
}

export interface UpdateActions {
  blip(): void;
  install(): void;
  later(): void;
}

/** "CubeCursor 0.2.0 is here": what's new, and a button to get it. */
export function buildUpdate(a: UpdateActions & { tour(): void }): ViewHost & { show(u: UpdateInfo): void; showNew(version: string, first: string, count: number): void; busy(text: string | null): void } {
  const title = h("div", { class: "u-title" });
  const notes = h("div", { class: "u-notes" });
  // "Update now", or (just updated) "See what's new".
  let fresh = false;
  const go = h("button", { class: "btn primary", text: "Update now", onclick: () => { a.blip(); if (fresh) a.tour(); else a.install(); } }) as HTMLButtonElement;
  const later = h("button", { class: "btn", text: "Later", onclick: () => { a.blip(); a.later(); } });
  const el = h("div", { class: "view update" }, h("div", { class: "u-text" }, title, notes, h("div", { class: "u-row" }, go, later)));
  return {
    el,
    sync() {},
    showNew(version, first, count) {
      fresh = true;
      title.textContent = `New in CubeCursor ${version}`;
      notes.textContent = count > 1 ? `${first}, and ${count - 1} more.` : first;
      go.disabled = false;
      go.textContent = "See what's new";
    },
    show(u) {
      fresh = false;
      title.textContent = `CubeCursor ${u.version} is here`;
      // The first couple of lines of what's new.
      notes.textContent = u.notes.split("\n").map((l) => l.replace(/^[-*•]\s*/, "").trim()).filter(Boolean).slice(0, 2).join(" · ") || "Fresh improvements and fixes.";
      go.disabled = false;
      go.textContent = "Update now";
    },
    busy(text) {
      go.disabled = text != null;
      go.textContent = text ?? "Update now";
    },
  };
}
