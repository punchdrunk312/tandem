// The island's views: header, home (status + actions + transfers), drop, note.
// Everything that comes from the network goes through text nodes.

import { fire } from "../bridge";
import type { Snapshot } from "../types";
import { h, svg } from "./dom";
import { fmtMins, momentText } from "./moments";
import { State, type LyricLine } from "./state";
import { nextReminder, reminderPresets, whenText } from "../core/notes";

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
  /** The header's music button: what is playing, or back. */
  toggleMedia(): void;
  /** The header's notes button: notes and reminders, or back. */
  toggleNotes(): void;
  toggleGame(): void;
  toggleAi(): void;
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
  notes: "M6 3h9l5 5v13H6zM14 3v6h6M9 13h7M9 17h5",
  music: "M9 18V5l12-2v13M9 18a3 3 0 11-6 0 3 3 0 016 0zM21 16a3 3 0 11-6 0 3 3 0 016 0z",
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

export const DEFAULT_ICONS = "ai,notes,music,timer,devices,game,sound";

export function buildHeader(a: ViewActions): ViewHost {
  const dot = h("i", { class: "hd-dot" });
  const title = h("span", { class: "hd-title", text: "CubeCursor" });
  const sound = h("button", { class: "icon-btn", title: "Sound", "aria-label": "Toggle sound", onclick: () => a.toggleSound() });
  const devices = h("button", { class: "icon-btn", title: "Your computers", "aria-label": "Show your computers", onclick: () => { a.blip(); a.toggleDevices(); } }, svg(ICON.devices, 15, { stroke: 1.8 }));
  const settings = h("button", { class: "icon-btn", title: "Pair & settings", "aria-label": "Open settings", onclick: () => { a.blip(); fire("open_main_window"); } }, svg(ICON.sliders, 15, { stroke: 1.8 }));
  const close = h("button", { class: "icon-btn", title: "Close", "aria-label": "Close", onclick: () => a.collapse() }, svg(ICON.close, 15, { stroke: 1.8 }));
  const timer = h("button", { class: "icon-btn", title: "Focus timer", "aria-label": "Focus timer", onclick: () => { a.blip(); a.toggleFocus(); } }, svg(ICON.timer, 15, { stroke: 1.8 }));
  const music = h("button", { class: "icon-btn", title: "Now playing", "aria-label": "Now playing", onclick: () => { a.blip(); a.toggleMedia(); } }, svg(ICON.music, 15, { stroke: 1.8 }));
  const notes = h("button", { class: "icon-btn", title: "Notes", "aria-label": "Notes and reminders", onclick: () => { a.blip(); a.toggleNotes(); } }, svg(ICON.notes, 15, { stroke: 1.8 }));
  const game = h("button", { class: "icon-btn", title: "Catch with Bob", "aria-label": "Play catch with Bob", onclick: () => { a.blip(); a.toggleGame(); } },
    svg("M12 3a9 9 0 100 18 9 9 0 000-18zM3.5 9.5h17M3.5 14.5h17M12 3c2.5 2.6 2.5 15.4 0 18", 15, { stroke: 1.8 }));
  const ai = h("button", { class: "icon-btn", title: "AI agents", "aria-label": "AI agents", onclick: () => { a.blip(); a.toggleAi(); } },
    svg("M12 3l1.8 4.6L18.5 9.4l-4.7 1.8L12 16l-1.8-4.8L5.5 9.4l4.7-1.8zM18.5 15l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8z", 15, { stroke: 1.8 }));
  const byId: Record<string, HTMLElement> = { ai, notes, music, timer, devices, game, sound };
  const el = h("div", { id: "header" }, dot, title, h("span", { class: "spacer" }));
  let order = "-";
  return {
    el,
    sync() {
      // Your icons, in your order (settings → Top bar icons).
      const want = State.settings.barIcons || DEFAULT_ICONS;
      if (want !== order) {
        order = want;
        const ids = want.split(",").filter((id) => byId[id]);
        el.replaceChildren(dot, title, h("span", { class: "spacer" }), ...ids.map((id) => byId[id]), settings, close);
      }
      const live = State.snap?.control && State.snap.control !== "offline";
      dot.className = `hd-dot${live ? " on" : ""}`;
      devices.classList.toggle("on", State.view === "devices");
      timer.classList.toggle("on", State.view === "focus" || !!State.focus);
      music.classList.toggle("on", State.view === "media" || !!State.media?.playing);
      notes.classList.toggle("on", State.view === "notes" || !!State.dueNote);
      game.classList.toggle("on", State.view === "game");
      ai.classList.toggle("on", State.view === "ai" || State.view === "agent");
      ai.classList.toggle("ping", [...agents.values()].some((e) => e.state === "approve" || e.state === "waiting"));
      title.textContent = State.view === "lyrics" ? "Lyrics" : State.view === "stats" ? "Your week in music" : State.view === "ai" ? "AI agents" : State.view === "game" ? "Catch" : State.view === "tour" ? "Welcome" : State.view === "devices" ? "Your computers" : State.view === "focus" ? "Focus" : State.view === "media" ? "Now playing" : State.view === "notes" ? "Notes" : State.view === "recipients" ? "Send to…" : State.greeting ?? "CubeCursor";
      sound.replaceChildren(svg(State.settings.soundEnabled ? ICON.speaker : ICON.speakerOff, 15, { stroke: 1.8 }));
    },
  };
}

// ------------------------------------------------------------------ suggestion

export interface Suggestion {
  title: string;
  sub: string;
  /** The button that does it (if any). */
  act?: [string, () => void];
}

/** Bob has an idea: a line, why, and a button to do it. */
export function buildSuggest(dismiss: () => void): ViewHost & { show(s: Suggestion): void } {
  const title = h("div", { class: "sg-title" });
  const sub = h("div", { class: "sg-sub" });
  const acts = h("div", { class: "m-actions" });
  const el = h("div", { class: "view suggest" }, h("div", { class: "sg-text" }, title, sub, acts));
  return {
    el,
    sync() {},
    show(s) {
      title.textContent = s.title;
      sub.textContent = s.sub;
      acts.replaceChildren(
        ...(s.act ? [h("button", { class: "btn primary", text: s.act[0], onclick: () => { s.act![1](); dismiss(); } })] : []),
        h("button", { class: "btn", text: s.act ? "Not now" : "Thanks, Bob", onclick: dismiss }),
      );
    },
  };
}

// ------------------------------------------------------------------ tour

export interface TourStep {
  title: string;
  sub: string;
}

/** The first-run tour: a step at a time, dots for where you are. */
export function buildTour(a: { next(): void; skip(): void }): ViewHost & { show(step: TourStep, i: number, n: number): void } {
  const dots = h("div", { class: "tr-dots" });
  const title = h("div", { class: "tr-title" });
  const sub = h("div", { class: "tr-sub" });
  const next = h("button", { class: "btn primary", onclick: () => a.next() });
  const skip = h("button", { class: "btn", text: "Skip", onclick: () => a.skip() });
  const text = h("div", { class: "tr-text" }, dots, title, sub, h("div", { class: "m-actions" }, next, skip));
  const el = h("div", { class: "view tour" }, text);
  return {
    el,
    sync() {},
    show(step, i, n) {
      dots.replaceChildren(...Array.from({ length: n }, (_, k) => h("i", { class: k === i ? "on" : k < i ? "done" : "" })));
      title.textContent = step.title;
      sub.textContent = step.sub;
      next.textContent = i === n - 1 ? "Let's go!" : "Next";
      skip.style.display = i === n - 1 ? "none" : "";
      // Each step slides in.
      text.classList.remove("in");
      void text.offsetWidth;
      text.classList.add("in");
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
              r.message ? h("div", { class: "x-msg", text: r.message }) : ""),
            // Cancel an outgoing send that's still going, or drop an incoming one left paused.
            (r.direction === "out" && r.state === "active") || (r.direction === "in" && r.state === "paused")
              ? h("button", { class: "x-cancel", title: r.state === "paused" ? "Drop this send" : "Cancel this send", "aria-label": "Cancel send", onclick: () => fire("cancel_transfer", { id: r.id }) }, "✕")
              : "")),
      );
      for (const r of rows) {
        const row = xfers.querySelector<HTMLElement>(`[data-id="${r.direction}${r.id}"]`);
        if (!row) continue;
        const pct = r.state === "done" ? 100 : r.total ? Math.min(100, (r.bytes / r.total) * 100) : 0;
        (row.querySelector(".x-bar > i") as HTMLElement).style.width = `${pct}%`;
        (row.querySelector(".x-state") as HTMLElement).textContent =
          r.state === "done" ? (r.direction === "out" ? "sent" : "received") : r.state === "error" ? "failed" : r.state === "paused" ? `paused · ${fmtBytes(r.bytes)} / ${fmtBytes(r.total)}` : `${fmtBytes(r.bytes)} / ${fmtBytes(r.total)}`;
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

/** The phone's slim pill: chargers, computers coming and going, earbuds connecting.
 *  An icon and a word on the left, the level (or the news) on the right. */
const SKY: Record<string, [string, string]> = {
  clear: ["☀️", "Clear"], clouds: ["☁️", "Cloudy"], fog: ["🌫️", "Foggy"], drizzle: ["🌦️", "Drizzle"],
  rain: ["🌧️", "Raining"], snow: ["❄️", "Snowing"], storm: ["⛈️", "Thunderstorm"],
};

export function buildCharge(): ViewHost & {
  show(m: import("./moments").Moment): void;
  showWeather(w: { city: string; temp: number; sky: string; day: boolean }): void;
} {
  const fill = h("i", { class: "ch-fill" });
  const bolt = svg(BOLT, 12);
  bolt.classList.add("ch-bolt");
  const batt = h("div", { class: "ch-batt" }, fill, bolt);
  const word = h("span", { class: "ch-word" });
  const pct = h("span", { class: "ch-pct" });
  const icon = h("div", { class: "ch-icon" });
  const el = h("div", { class: "view charge" }, h("div", { class: "ch-left" }, icon, word), pct);
  return {
    el,
    sync() {},
    showWeather(w) {
      el.className = `view charge${State.view === "charge" ? " on" : ""} k-weather sky-${w.sky}`;
      void el.offsetWidth;
      el.classList.add("enter");
      const [emoji, what] = SKY[w.sky] ?? SKY.clouds;
      icon.replaceChildren(h("span", { class: "ch-emoji", text: w.sky === "clear" && !w.day ? "🌙" : emoji }));
      word.textContent = `${w.city} · ${what}`;
      pct.textContent = `${w.temp}°`;
    },
    show(m) {
      const me = m.id === "me";
      const acc = m.kind === "acc-on" ? m.acc : undefined;
      const level = acc ? acc.battery ?? 0 : m.device?.battery ?? 0;
      const low = level <= 20 && (m.kind === "unplug" || !!acc);
      el.className = `view charge${State.view === "charge" ? " on" : ""} k-${m.kind}${low ? " low" : ""}`;
      el.style.setProperty("--c", m.color);
      void el.offsetWidth;
      el.classList.add("enter");
      icon.replaceChildren();
      if (m.kind === "online" || m.kind === "offline") {
        // A computer arriving or leaving: its letter in its colour, and the news.
        icon.append(h("div", { class: "ch-avatar", text: Array.from(m.name)[0]?.toUpperCase() ?? "?" }));
        word.textContent = m.name;
        pct.textContent = m.kind === "online" ? "Connected" : "Away";
        return;
      }
      if (acc) {
        // Earbuds: a small ring, the name, and each bud's level.
        const r = ring(22, 3);
        r.set(level, low ? "#ff8a5c" : "#34d399", null, "");
        icon.append(r.el);
        word.textContent = acc.name;
        pct.textContent = acc.left != null || acc.right != null
          ? [acc.left != null ? `L ${acc.left}%` : "", acc.right != null ? `R ${acc.right}%` : ""].filter(Boolean).join("  ")
          : `${level}%`;
        return;
      }
      icon.append(batt);
      const what = m.kind === "plug" ? "Charging" : m.kind === "full" ? "Fully charged" : "On battery";
      word.textContent = me ? what : `${m.name} · ${what.toLowerCase()}`;
      pct.textContent = `${level}%`;
      fill.style.width = "0%";
      // The level fills in once the pill has opened.
      requestAnimationFrame(() => requestAnimationFrame(() => (fill.style.width = `${Math.max(4, level)}%`)));
    },
  };
}

// ------------------------------------------------------------------ device cards

export interface DeviceActions {
  jump(peerId: string): void;
  power(peerId: string, action: "lock" | "sleep"): void;
  /** Play that computer's sound here (or stop). */
  hear(peerId: string, on: boolean): void;
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
                    ? [btn("Jump", () => a.jump(c.id), "primary"),
                       btn(State.snap?.audio_in === c.id ? "🔊 Stop sound" : "🔊 Hear it", () => a.hear(c.id, State.snap?.audio_in !== c.id)),
                       btn("Lock", () => a.power(c.id, "lock")), btn("Sleep", () => a.power(c.id, "sleep"))]
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
          // Any length from 5 minutes to 4 hours: the bar can't be typed into, so − and +.
          const set = (n: number) => { State.settings.focusMinutes = Math.min(240, Math.max(5, n)); State.saveSettings(); State.notify(); };
          const step = m >= 60 ? 15 : 5;
          const chip = (n: number) => btn(`${n} min`, () => set(n), n === m ? "on" : "");
          row.replaceChildren(
            btn("−", () => set(m - (m > 60 ? 15 : 5)), "f-step"),
            btn(m >= 60 ? `${Math.floor(m / 60)} h${m % 60 ? ` ${m % 60}` : ""}` : `${m} min`, () => {}, "on f-len"),
            btn("+", () => set(m + step), "f-step"),
            ...[25, 50].filter((n) => n !== m).map(chip),
            btn("Start", () => a.start(State.settings.focusMinutes), "primary"));
        }
      }
    },
  };
}


// ------------------------------------------------------------------ now playing

export type MediaAction = "play_pause" | "next" | "prev";

export interface MediaActions {
  blip(): void;
  control(action: MediaAction): void;
  /** Jump to this spot in the song (ms). */
  seek(ms: number): void;
  toggleLyrics(): void;
}

/** The media view's extra row: app volume, where the sound goes, speed, sleep timer. */
export interface MediaMore {
  audio(): Promise<{ volume: number | null; outputs: { id: string; name: string; default: boolean }[] }>;
  setVolume(v: number): void;
  setOutput(id: string): void;
  speed(): number;
  setSpeed(r: number): void;
  /** Minutes left on the sleep timer (0: off). */
  sleepLeft(): number;
  cycleSleep(): void;
  openLyrics(): void;
  openStats(): void;
  /** Spotify (if connected): the song, liked or not, and what's next. */
  spotify(): Promise<SpotifyNow | null>;
  like(id: string, on: boolean): Promise<void>;
  playlists(): Promise<{ id: string; name: string }[]>;
  addTo(playlist: string, id: string, name: string): void;
  /** Opens Spotify's sign-in in the browser. */
  connectSpotify(): void;
}
export interface SpotifyNow { id: string; name: string; artist: string; liked: boolean; next: { name: string; artist: string }[] }

/** Where the song is now: the last position Windows gave, moved on while it plays. */
export function mediaPosition(): number {
  const m = State.media;
  if (!m) return 0;
  const p = m.position_ms + (m.playing ? performance.now() - State.mediaAt : 0);
  return m.duration_ms ? Math.min(p, m.duration_ms) : p;
}

/** "[01:02.34] words" lines → time-stamped lines, in order (a line may carry several stamps). */
export function parseLrc(text: string): LyricLine[] {
  const out: LyricLine[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const stamps = [...raw.matchAll(/\[(\d+):(\d+(?:\.\d+)?)\]/g)];
    if (!stamps.length) continue;
    const words = raw.replace(/\[[^\]]*\]/g, "").trim();
    for (const st of stamps) out.push({ ms: Math.round((Number(st[1]) * 60 + Number(st[2])) * 1000), text: words });
  }
  return timeWords(out.sort((x, y) => x.ms - y.ms));
}

/** When each word of each line is sung. The libraries time lines, not words, so the words
 *  share the line's time by length, at a singing pace (a line followed by a long pause is
 *  sung in the first part of it, not stretched over the whole gap). */
export function timeWords(lines: LyricLine[]): LyricLine[] {
  return lines.map((l, i) => {
    const words = l.text.split(/\s+/).filter(Boolean);
    if (!words.length) return l;
    const gap = (lines[i + 1]?.ms ?? l.ms + 6000) - l.ms;
    const span = Math.max(300, Math.min(gap * 0.92, words.length * 420 + 350));
    const weight = words.map((w) => w.length + 2);
    const total = weight.reduce((a, b) => a + b, 0);
    let at = l.ms;
    return {
      ...l,
      words: words.map((text, k) => {
        const d = (span * weight[k]) / total;
        const w = { ms: Math.round(at), end: Math.round(at + d), text };
        at += d;
        return w;
      }),
    };
  });
}

/** Lyrics without timing, spread evenly over the song (from 6% to 94% of it) so they
 *  still follow along roughly. Blank lines and "♪" markers are dropped. */
export function spreadLyrics(text: string, durationMs: number): LyricLine[] {
  const lines = text.split(/\r?\n/).map((l) => l.trim()).filter((l) => l && l !== "♪");
  if (!lines.length || durationMs <= 0) return [];
  const from = durationMs * 0.06, span = durationMs * 0.88;
  return timeWords(lines.map((t, i) => ({ ms: Math.round(from + (span * i) / lines.length), text: t })));
}

/** The lyric line being sung now, and the one after it. */
export function lyricAt(ms: number): { now: string; next: string; line: LyricLine | null } {
  const l = State.lyrics;
  if (!Array.isArray(l) || !l.length) return { now: "", next: "", line: null };
  let i = -1;
  for (let k = 0; k < l.length && l[k].ms <= ms; k++) i = k;
  return { now: i >= 0 ? l[i].text : "", next: l[i + 1]?.text ?? "", line: i >= 0 ? l[i] : null };
}

/** A lyric line sung word by word: sung words light up, the word being sung fills from
 *  left to right, and a new line slides in from below. A line too long for the space keeps
 *  the word being sung in view. `show(ms)` runs every frame, so it only touches styles. */
export function karaoke(cls: string): { el: HTMLElement; show(ms: number, fallback: string): boolean } {
  const el = h("div", { class: `kar ${cls}` });
  let current: LyricLine | null | undefined;
  let fallbackShown = "";
  let row: HTMLElement | null = null;
  let spans: HTMLElement[] = [];
  let shift = 0;
  const swap = (next: HTMLElement) => {
    const old = row;
    if (old) {
      old.classList.add("out");
      window.setTimeout(() => old.remove(), 260);
    }
    row = next;
    el.append(next);
    shift = 0;
  };
  return {
    el,
    show(ms, fallback) {
      const { line } = lyricAt(ms);
      // Between lines (an instrumental bit): the fallback (the artist), quietly.
      const sung = line && line.words?.length && ms < (line.words.at(-1)!.end + 2500) ? line : null;
      if (!sung) {
        if (current !== null || fallbackShown !== fallback) {
          current = null;
          fallbackShown = fallback;
          spans = [];
          swap(h("div", { class: "kar-row rest", text: fallback }));
        }
        return false;
      }
      if (sung !== current) {
        current = sung;
        spans = sung.words!.map((w) => h("span", { text: w.text }));
        const r = h("div", { class: "kar-row" });
        spans.forEach((s, k) => r.append(...(k ? [" ", s] : [s])));
        swap(r);
        // A little too long: a touch smaller. Much too long: it follows the singing.
        const over = r.scrollWidth / Math.max(1, el.clientWidth);
        if (over > 1 && over < 1.22) r.style.fontSize = `${Math.max(0.82, 1 / over)}em`;
      }
      let now: HTMLElement | null = null;
      sung.words!.forEach((w, k) => {
        const s = spans[k];
        const p = Math.min(1, Math.max(0, (ms - w.ms) / Math.max(1, w.end - w.ms)));
        const state = p >= 1 ? "sung" : p > 0 ? "now" : "";
        if (s.className !== state) s.className = state;
        if (state === "now") {
          s.style.setProperty("--p", `${(p * 100).toFixed(1)}%`);
          now = s;
        }
      });
      if (row && row.scrollWidth > el.clientWidth + 1) {
        const at = now ?? [...spans].reverse().find((s) => s.className === "sung") ?? spans[0];
        const want = Math.max(0, Math.min(row.scrollWidth - el.clientWidth, at.offsetLeft + at.offsetWidth / 2 - el.clientWidth * 0.45));
        if (Math.abs(want - shift) > 2) {
          shift = want;
          row.style.transform = `translateX(${-shift}px)`;
        }
      }
      return true;
    },
  };
}

/** A song's progress you can click or drag along to jump there. */
function seekBar(cls: string, a: MediaActions): { el: HTMLElement; show(ms: number, total: number, can: boolean): void } {
  const fill = h("i");
  const knob = h("b");
  const el = h("div", { class: `seek ${cls}`, role: "slider", "aria-label": "Song position" }, fill, knob);
  let dragging = false;
  const frac = (e: PointerEvent) => {
    const r = el.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
  };
  const paint = (f: number) => {
    fill.style.width = `${f * 100}%`;
    knob.style.left = `${f * 100}%`;
  };
  el.addEventListener("pointerdown", (e) => {
    if (!el.classList.contains("can")) return;
    dragging = true;
    el.setPointerCapture(e.pointerId);
    el.classList.add("drag");
    paint(frac(e));
  });
  el.addEventListener("pointermove", (e) => { if (dragging) paint(frac(e)); });
  el.addEventListener("pointerup", (e) => {
    if (!dragging) return;
    dragging = false;
    el.classList.remove("drag");
    const total = State.media?.duration_ms ?? 0;
    if (total) { a.blip(); a.seek(frac(e) * total); }
  });
  return {
    el,
    show(ms, total, can) {
      el.classList.toggle("can", can && total > 0);
      el.title = can ? "Click or drag to jump" : "This player doesn't let CubeCursor jump around in the song";
      if (!dragging) paint(total ? Math.min(1, ms / total) : 0);
    },
  };
}

/** "Spotify" from "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify" or "Spotify.exe". */
export function appName(id: string): string {
  const last = id.split("!").pop() ?? id;
  const base = last.replace(/\.exe$/i, "").split(".").pop() ?? last;
  if (/^msedge$/i.test(base)) return "Edge";
  return base ? base[0].toUpperCase() + base.slice(1) : "";
}

const MEDIA_ICON = {
  back15: "M11 5L6 9l5 4M6 9h8a5 5 0 0 1 0 10h-3",
  fwd15: "M13 5l5 4-5 4M18 9h-8a5 5 0 0 0 0 10h3",
  prev: "M6 5v14M19 5L9 12l10 7z",
  next: "M18 5v14M5 5l10 7-10 7z",
  play: "M7 4l13 8-13 8z",
  pause: "M7 4h4v16H7zM14 4h4v16h-4z",
};

/** The song, how far along it is, play/pause/skip, and (if wanted) the lyric being sung. */
export function buildMedia(a: MediaActions & MediaMore): ViewHost {
  const app = h("div", { class: "m-app" });
  const title = h("div", { class: "m-title" });
  const artist = h("div", { class: "m-artist" });
  const bar = seekBar("m-bar", a);
  const tNow = h("span"), tEnd = h("span");
  const times = h("div", { class: "m-times" }, tNow, tEnd);
  const icon = (d: string, filled = true) => svg(d, 18, filled ? { fill: "currentColor", stroke: 0 } : { stroke: 2 });
  const btn = (label: string, d: string, act: MediaAction) =>
    h("button", { class: "icon-btn m-btn", title: label, "aria-label": label, onclick: () => { a.blip(); a.control(act); } }, icon(d));
  const jump = (label: string, d: string, by: number) =>
    h("button", { class: "icon-btn m-btn m-jump", title: label, "aria-label": label, onclick: () => { a.blip(); a.seek(Math.max(0, mediaPosition() + by)); } }, icon(d, false));
  const prev = btn("Previous (or the start of this song)", MEDIA_ICON.prev, "prev");
  const back = jump("15 seconds back", MEDIA_ICON.back15, -15_000);
  const play = btn("Play / pause", MEDIA_ICON.play, "play_pause");
  const fwd = jump("15 seconds on", MEDIA_ICON.fwd15, 15_000);
  const next = btn("Next", MEDIA_ICON.next, "next");
  const lyr = h("button", { class: "btn m-lyr", text: "Lyrics", title: "Show lyrics (looks the song up online)", onclick: () => { a.blip(); a.toggleLyrics(); } });
  const sing = karaoke("m-sing");
  const line = h("div", { class: "m-line" });
  const lineNext = h("div", { class: "m-line next" });
  const empty = h("div", { class: "m-empty", text: "Nothing is playing. Start a song in Spotify, a browser or any player, and it shows up here." });
  // The second row: the app's own volume, where the sound comes out, speed, sleep, more.
  const vol = h("input", { type: "range", class: "m-vol", min: "0", max: "1", step: "0.02", title: "This app's volume", "aria-label": "App volume" }) as HTMLInputElement;
  let volT = 0;
  vol.oninput = () => { window.clearTimeout(volT); volT = window.setTimeout(() => a.setVolume(Number(vol.value)), 40); };
  const out = h("select", { class: "m-out", title: "Where the sound comes out", "aria-label": "Sound output" }) as HTMLSelectElement;
  out.onchange = () => { a.blip(); a.setOutput(out.value); };
  const speed = h("button", { class: "btn m-chip", title: "Playback speed", onclick: () => {
    const steps = [1, 1.25, 1.5, 2, 0.75];
    a.blip();
    a.setSpeed(steps[(steps.indexOf(a.speed()) + 1) % steps.length]);
  } });
  const sleep = h("button", { class: "btn m-chip", title: "Sleep timer: the music stops by itself", onclick: () => { a.blip(); a.cycleSleep(); } });
  const full = h("button", { class: "btn m-chip", text: "All lyrics", title: "The whole song's words", onclick: () => { a.blip(); a.openLyrics(); } });
  const stats = h("button", { class: "btn m-chip", text: "📊", title: "What you listened to this week", onclick: () => { a.blip(); a.openStats(); } });
  // Spotify: the heart, "add to a playlist", and what's next (beside the app's name).
  let sp: SpotifyNow | null = null;
  const heart = h("button", { class: "icon-btn m-btn m-heart", hidden: true, title: "Save to your Liked Songs", "aria-label": "Like", onclick: () => {
    if (!sp) {
      a.connectSpotify();
      return;
    }
    const s = sp;
    a.blip();
    s.liked = !s.liked;
    paintHeart();
    void a.like(s.id, s.liked).catch(() => { s.liked = !s.liked; paintHeart(); });
  } });
  const paintHeart = () => {
    heart.replaceChildren(svg("M12 20s-7-4.4-7-10a4 4 0 0 1 7-2.6A4 4 0 0 1 19 10c0 5.6-7 10-7 10z", 17, sp?.liked ? { fill: "#1ed760", stroke: 0 } : { stroke: 1.9 }));
    heart.classList.toggle("on", !!sp?.liked);
    heart.title = sp ? "Save to your Liked Songs" : "Connect Spotify to like songs and add them to playlists";
  };
  const plist = h("select", { class: "m-pl", hidden: true, title: "Add this song to a playlist", "aria-label": "Add to playlist" }) as HTMLSelectElement;
  plist.onfocus = () => {
    if (plist.options.length > 1) return;
    void a.playlists().then((ls) => plist.append(...ls.map((p) => h("option", { value: p.id, text: p.name }))), () => {});
  };
  plist.onchange = () => {
    if (sp && plist.value) a.addTo(plist.value, sp.id, plist.selectedOptions[0]?.text ?? "");
    plist.value = "";
  };
  plist.append(h("option", { value: "", text: "＋", hidden: true }));
  const upnext = h("span", { class: "m-next" });
  let spFor = "";
  let spAt = 0;
  const loadSpotify = (m: NonNullable<typeof State.media>) => {
    const key = `${m.title}|${m.artist}`;
    if (!/spotify/i.test(m.app) || (key === spFor && performance.now() - spAt < 30_000)) return;
    spFor = key;
    spAt = performance.now();
    void a.spotify().then((n) => {
      sp = n;
      heart.hidden = false;
      plist.hidden = !n;
      paintHeart();
      upnext.textContent = n?.next[0] ? ` · Up next: ${n.next[0].name} — ${n.next[0].artist}` : "";
      upnext.title = n?.next.map((x, i) => `${i + 1}. ${x.name} — ${x.artist}`).join("\n") ?? "";
    }, (e) => {
      // Not signed in yet: the heart stays, and pressing it signs in.
      sp = null;
      heart.hidden = !/isn't connected/.test(String(e));
      plist.hidden = true;
      upnext.textContent = "";
      paintHeart();
    });
  };
  const row2 = h("div", { class: "m-row2" }, svg("M4 9v6h4l5 4V5L8 9H4z", 14, { stroke: 1.8 }), vol, out, speed, sleep, full, stats);
  let wasOpen = false;
  const loadAudio = () => void a.audio().then(({ volume, outputs }) => {
    vol.disabled = volume == null;
    if (volume != null) vol.value = String(volume);
    const peers = State.connectedPeers.filter((p) => !p.fleet);
    const sel = State.snap?.audio_out ? `pc:${State.snap.audio_out}` : outputs.find((o) => o.default)?.id ?? "";
    out.replaceChildren(
      ...outputs.map((o) => h("option", { value: o.id, text: o.name || "Speakers" })),
      ...peers.map((p) => h("option", { value: `pc:${p.id}`, text: `🖥 ${p.name}` })));
    out.value = sel;
  });
  const body = h("div", { class: "m-text" }, h("div", { class: "m-app-row" }, app, upnext), title, artist, h("div", { class: "m-row" }, heart, plist, prev, back, play, fwd, next, lyr), bar.el, times, sing.el, line, lineNext, row2);
  const el = h("div", { class: "view media" }, body, empty);
  const mmss = (ms: number) => {
    const s = Math.floor(ms / 1000);
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  };
  let shownPlaying: boolean | null = null;
  return {
    el,
    sync() {
      const m = State.media;
      body.hidden = !m;
      empty.hidden = !!m;
      const open = State.view === "media";
      if (open && !wasOpen) loadAudio();
      wasOpen = open;
      if (!m) return;
      if (open) loadSpotify(m);
      if (!/spotify/i.test(m.app) && !heart.hidden) { heart.hidden = plist.hidden = true; upnext.textContent = ""; sp = null; }
      const r = a.speed();
      speed.textContent = `${r}×`;
      speed.classList.toggle("on", r !== 1);
      const left = a.sleepLeft();
      sleep.textContent = left ? `🌙 ${left} min` : "🌙";
      sleep.classList.toggle("on", left > 0);
      app.textContent = appName(m.app);
      title.textContent = m.title;
      artist.textContent = [m.artist, m.album].filter(Boolean).join(" · ");
      prev.disabled = !m.can_prev;
      next.disabled = !m.can_next;
      if (shownPlaying !== m.playing) {
        shownPlaying = m.playing;
        play.replaceChildren(icon(m.playing ? MEDIA_ICON.pause : MEDIA_ICON.play));
      }
      const pos = mediaPosition();
      bar.show(pos, m.duration_ms, !!m.can_seek);
      back.disabled = fwd.disabled = !m.can_seek;
      tNow.textContent = mmss(pos);
      tEnd.textContent = m.duration_ms ? mmss(m.duration_ms) : "";
      lyr.classList.toggle("on", State.settings.lyrics);
      const timed = State.settings.lyrics && Array.isArray(State.lyrics);
      sing.el.hidden = !timed;
      line.hidden = timed;
      if (!State.settings.lyrics) {
        line.textContent = "";
        lineNext.textContent = "";
      } else if (typeof State.lyrics === "string") {
        line.textContent = State.lyrics.split("\n").find((s) => s.trim()) ?? "";
        lineNext.textContent = "No timing for this song, so the lyrics don't follow along.";
      } else if (Array.isArray(State.lyrics)) {
        sing.show(pos, "♪");
        lineNext.textContent = lyricAt(pos).next;
      } else {
        const key = `${m.title}|${m.artist}`;
        line.textContent = State.lyricsFor === key ? "No lyrics found for this song."
          : State.lyricsFor === `${key}|offline` ? "Couldn't reach the lyrics library. Try again later." : "Looking up the lyrics…";
        lineNext.textContent = "";
      }
    },
  };
}

export interface RecipientActions {
  blip(): void;
  toggle(id: string): void;
  /** Tick every computer (or, if all are ticked, none). */
  all(): void;
  send(): void;
  cancel(): void;
}

/** After a drop with several computers connected: tick who gets it, then send to all of
 *  them at once. Desk computers first, then the fleet. */
export function buildRecipients(a: RecipientActions): ViewHost {
  const grid = h("div", { class: "rc-grid" });
  const all = h("button", { class: "btn", onclick: () => { a.blip(); a.all(); } });
  const send = h("button", { class: "btn primary", onclick: () => { a.blip(); a.send(); } });
  const cancel = h("button", { class: "btn", text: "Cancel", onclick: () => { a.blip(); a.cancel(); } });
  const el = h("div", { class: "view recipients" }, grid, h("div", { class: "rc-row" }, all, cancel, send));
  return {
    el,
    sync() {
      const peers = [...State.connectedPeers].sort((x, y) => Number(!!x.fleet) - Number(!!y.fleet) || x.name.localeCompare(y.name));
      const key = JSON.stringify([peers.map((p) => [p.id, p.name, p.fleet]), [...State.picked]]);
      if (grid.dataset.key !== key) {
        grid.dataset.key = key;
        grid.replaceChildren(...peers.map((p) => h("button", {
          class: `rc-chip${State.picked.has(p.id) ? " on" : ""}`,
          title: p.fleet ? `${p.name} (fleet: files only)` : p.name,
          onclick: () => { a.blip(); a.toggle(p.id); },
        }, h("i", { class: "rc-dot", style: `background:${p.color ?? "var(--teal)"}` }), h("span", { text: p.name }), ...(p.fleet ? [h("small", { text: "fleet" })] : []))));
      }
      const n = [...State.picked].filter((id) => peers.some((p) => p.id === id)).length;
      all.textContent = n === peers.length ? "None" : "All";
      send.textContent = n === 0 ? "Pick computers" : `Send to ${n}`;
      (send as HTMLButtonElement).disabled = n === 0;
    },
  };
}

/** A YouTube video playing on in the bar (its browser is minimised): the video, and
 *  buttons to send it back to the browser, on to another computer, or close it. Reports
 *  how far it has got. */
export function buildVideo(a: { blip(): void; back(): void; close(): void; sendTo(peerId: string): void }): ViewHost {
  const frame = h("div", { class: "vd-frame" });
  const back = h("button", { class: "icon-btn vd-btn", title: "Back to the browser", "aria-label": "Back to the browser", onclick: () => { a.blip(); a.back(); } },
    svg("M4 5h16v11H4zM8 20h8M12 16v4", 14, { stroke: 2 }));
  const close = h("button", { class: "icon-btn vd-btn", title: "Close (stays paused in the browser)", "aria-label": "Close", onclick: () => { a.blip(); a.close(); } },
    svg("M6 6l12 12M18 6L6 18", 12, { stroke: 2 }));
  const away = h("button", { class: "icon-btn vd-btn vd-away", "aria-label": "Play it on another computer", onclick: () => {
    const p = State.connectedPeers.find((x) => !x.fleet);
    if (p) { a.blip(); a.sendTo(p.id); }
  } }, svg("M3 5h12v9H3zM7 18h4M9 14v4M15 12h6m-3-3 3 3-3 3", 14, { stroke: 2 }));
  const el = h("div", { class: "view video" }, frame, h("div", { class: "vd-row" }, away, back, close));
  let shown = "";
  let player: HTMLIFrameElement | null = null;
  // The YouTube player says where it is when asked to "listen".
  window.addEventListener("message", (e) => {
    if (!player || e.source !== player.contentWindow || typeof e.data !== "string") return;
    try {
      const d = JSON.parse(e.data) as { event?: string; info?: { currentTime?: number } };
      const t = d.info?.currentTime;
      if (d.event === "infoDelivery" && typeof t === "number" && State.video) State.video.nowMs = Math.round(t * 1000);
    } catch { /* not ours */ }
  });
  return {
    el,
    sync() {
      const peer = State.connectedPeers.find((x) => !x.fleet);
      away.hidden = !peer;
      away.title = peer ? `Play it on ${peer.name}` : "";
      const v = State.video;
      const key = v ? `${v.id}@${v.startMs}` : "";
      if (key === shown) return;
      shown = key;
      player = null;
      frame.replaceChildren();
      if (!v) return;
      const src = `https://www.youtube-nocookie.com/embed/${encodeURIComponent(v.id)}?autoplay=1&playsinline=1&enablejsapi=1&start=${Math.floor(v.startMs / 1000)}&origin=${encodeURIComponent(location.origin)}`;
      const f = h("iframe", { src, title: v.title, allow: "autoplay; encrypted-media; picture-in-picture; fullscreen", referrerpolicy: "strict-origin-when-cross-origin", allowfullscreen: "" }) as HTMLIFrameElement;
      f.addEventListener("load", () => f.contentWindow?.postMessage(JSON.stringify({ event: "listening", id: 1 }), "*"));
      player = f;
      frame.append(f);
    },
  };
}

/** The slim strip that stays while a song plays: the song (or the words being sung),
 *  15 s back, play/pause, next, lyrics on/off, hide, and a thin bar to jump along it. */
export function buildNowPlaying(a: MediaActions & { open(): void; close(): void }): ViewHost {
  const title = h("div", { class: "np-title" });
  const sing = karaoke("np-sing");
  const icon = (d: string) => svg(d, 14, { fill: "currentColor" });
  const b = (label: string, child: Element, act: () => void, cls = "") =>
    h("button", { class: `icon-btn np-btn ${cls}`, title: label, "aria-label": label, onclick: () => { a.blip(); act(); } }, child);
  const back = b("15 seconds back", svg(MEDIA_ICON.back15, 15, { stroke: 2 }), () => a.seek(Math.max(0, mediaPosition() - 15_000)));
  const play = b("Play / pause", icon(MEDIA_ICON.play), () => a.control("play_pause"));
  const next = b("Next", icon(MEDIA_ICON.next), () => a.control("next"));
  const lyr = b("Lyrics", svg("M4 6h16M4 11h16M4 16h10M18 15v5", 14, { stroke: 2 }), () => a.toggleLyrics(), "np-lyr");
  const close = b("Hide until the next song", svg("M6 6l12 12M18 6L6 18", 12, { stroke: 2 }), () => a.close());
  const bar = seekBar("np-seek", a);
  const text = h("button", { class: "np-text", title: "Show more", onclick: () => { a.blip(); a.open(); } }, title, sing.el);
  const el = h("div", { class: "view nowplaying" }, text, h("div", { class: "np-row" }, back, play, next, lyr, close), bar.el);
  let shown: boolean | null = null;
  return {
    el,
    sync() {
      const m = State.media;
      if (!m) return;
      const pos = mediaPosition();
      lyr.classList.toggle("on", State.settings.lyrics);
      // With lyrics on, the second line sings along word by word; otherwise it names the artist.
      const timed = State.settings.lyrics && Array.isArray(State.lyrics);
      const singing = timed ? sing.show(pos, m.artist) : sing.show(-1, m.artist);
      title.textContent = singing ? `${m.title} · ${m.artist}` : m.title;
      el.classList.toggle("singing", singing);
      next.disabled = !m.can_next;
      back.disabled = !m.can_seek;
      bar.show(pos, m.duration_ms, !!m.can_seek);
      if (shown !== m.playing) {
        shown = m.playing;
        play.replaceChildren(icon(m.playing ? MEDIA_ICON.pause : MEDIA_ICON.play));
      }
    },
  };
}

/** A lively colour from a picture (the album art): the average of its brighter, more
 *  colourful pixels, lifted so it reads as a tint. "" when there is nothing to go on. */
export async function pictureColor(url: string): Promise<string> {
  if (!url) return "";
  const img = new Image();
  img.src = url;
  try { await img.decode(); } catch { return ""; }
  const c = document.createElement("canvas");
  c.width = c.height = 24;
  const x = c.getContext("2d", { willReadFrequently: true });
  if (!x) return "";
  x.drawImage(img, 0, 0, 24, 24);
  const px = x.getImageData(0, 0, 24, 24).data;
  let r = 0, g = 0, b = 0, w = 0;
  for (let i = 0; i < px.length; i += 4) {
    const R = px[i], G = px[i + 1], B = px[i + 2];
    const max = Math.max(R, G, B), min = Math.min(R, G, B);
    // Favour saturated, not-too-dark pixels; greys count a little.
    const weight = (max - min) / 255 * (max / 255) + 0.02;
    r += R * weight; g += G * weight; b += B * weight; w += weight;
  }
  if (!w) return "";
  [r, g, b] = [r / w, g / w, b / w];
  const top = Math.max(r, g, b, 1);
  const lift = Math.min(1.6, 200 / top);
  return `rgb(${Math.round(Math.min(255, r * lift))}, ${Math.round(Math.min(255, g * lift))}, ${Math.round(Math.min(255, b * lift))})`;
}

// ------------------------------------------------------------------ notes & reminders

export interface NotesActions {
  blip(): void;
  /** Writing: the bar takes the keyboard (on) or gives it back (off). */
  typing(on: boolean): void;
  add(text: string, remindAt: number | null): void;
  done(id: string): void;
  snooze(id: string, minutes: number): void;
}

/** A reminder that is due, or the notes to do with a way to write a new one, all in the bar. */
export function buildNotes(a: NotesActions): ViewHost & { stopWriting(): void } {
  const big = h("div", { class: "nt-big" });
  const sub = h("div", { class: "nt-sub" });
  const list = h("div", { class: "nt-list" });
  const row = h("div", { class: "nt-row" });
  const input = h("input", { class: "nt-input", type: "text", maxlength: 300, placeholder: "Write a note…", spellcheck: true }) as HTMLInputElement;
  const when = h("div", { class: "nt-when" });
  const write = h("div", { class: "nt-write" }, input, when);
  const el = h("div", { class: "view notes" }, h("div", { class: "nt-text" }, big, sub, list, write, row));
  const btn = (label: string, onclick: () => void, cls = "") => h("button", { class: `btn ${cls}`.trim(), text: label, onclick: () => { a.blip(); onclick(); } });
  let writing = false;
  let remindAt: number | null = null;
  const stop = () => {
    if (!writing) return;
    writing = false;
    input.value = "";
    remindAt = null;
    a.typing(false);
    el.dataset.key = "";
  };
  const submit = () => {
    if (input.value.trim()) a.add(input.value, remindAt);
    stop();
    State.notify();
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") submit();
    if (e.key === "Escape") { stop(); State.notify(); }
  });
  const chips = () => {
    const opts: { label: string; at: number | null }[] = [{ label: "No reminder", at: null }, ...reminderPresets()];
    when.replaceChildren(...opts.map((o) => {
      const on = o.at === null ? remindAt === null : remindAt !== null && Math.abs(remindAt - o.at) < 60_000;
      return btn(o.label, () => { remindAt = o.at; chips(); input.focus({ preventScroll: true }); }, `chip${on ? " on" : ""}`);
    }));
  };
  return {
    el,
    stopWriting: stop,
    sync() {
      write.hidden = !writing;
      if (writing) {
        big.textContent = "";
        sub.textContent = remindAt != null ? `⏰ Bob reminds you ${whenText(remindAt).toLowerCase()}` : "Enter to keep it · Esc to cancel";
        list.replaceChildren();
        if (el.dataset.key !== "write") {
          el.dataset.key = "write";
          chips();
          row.replaceChildren(btn("Keep note", submit, "primary"), btn("Cancel", () => { stop(); State.notify(); }));
        }
        return;
      }
      const notes = State.notes.filter((n) => !n.done);
      const due = State.notes.find((n) => n.id === State.dueNote && !n.done);
      const next = nextReminder(notes);
      const key = JSON.stringify([due?.id, due?.text, notes.map((n) => [n.id, n.text, n.remindAt]), Math.floor(Date.now() / 60_000)]);
      if (el.dataset.key === key) return;
      el.dataset.key = key;
      const startWriting = () => {
        writing = true;
        a.typing(true);
        State.notify();
        // The window takes the keyboard a moment after asking.
        window.setTimeout(() => input.focus({ preventScroll: true }), 60);
      };
      if (due) {
        big.textContent = due.text;
        sub.textContent = "⏰ Reminder";
        list.replaceChildren();
        row.replaceChildren(btn("Done", () => a.done(due.id), "primary"), btn("In 10 min", () => a.snooze(due.id, 10)), btn("In 1 hour", () => a.snooze(due.id, 60)));
        return;
      }
      if (!notes.length) {
        big.textContent = "Nothing to remember";
        sub.textContent = "Write a note here, and Bob reminds you when it's time.";
        list.replaceChildren();
        row.replaceChildren(btn("Write a note", startWriting, "primary"));
        return;
      }
      const top = next ?? notes[0];
      big.textContent = top.text;
      sub.textContent = top.remindAt != null ? `⏰ ${whenText(top.remindAt)}` : `${notes.length} note${notes.length === 1 ? "" : "s"} to do`;
      const others = notes.filter((n) => n.id !== top.id).slice(0, 2);
      list.replaceChildren(...others.map((n) => h("div", { class: "nt-item" },
        h("button", { class: "nt-tick", title: "Done", "aria-label": "Done", onclick: () => { a.blip(); a.done(n.id); } }),
        h("span", { text: `${n.text}${n.remindAt != null ? `  ·  ${whenText(n.remindAt)}` : ""}` }))));
      row.replaceChildren(btn("Done", () => a.done(top.id)), btn("Write a note", startWriting, "primary"));
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

// ------------------------------------------------------------------ AI agents

/** An event from an AI agent (Claude Code, Codex) on any of your computers. */
export interface AgentEv {
  id: string; pc: string; pcName: string; source: string; session: string;
  project: string; state: string; text: string; tool: string; at: number;
}
/** The agents we know of now, by computer + tool + session. */
export const agents = new Map<string, AgentEv>();
export const agentKey = (e: AgentEv) => `${e.pc}|${e.source}|${e.session}`;
export const agentName = (s: string) => (s === "claude" ? "Claude Code" : s === "codex" ? "Codex" : s);
const STATE_WORDS: Record<string, string> = { working: "Working…", waiting: "Needs you", approve: "Asks to do something", done: "Done", allowed: "Working…", denied: "Working…", terminal: "Asking in the terminal" };

export interface Prompt { id: string; title: string; text: string; at: number; del?: boolean }
export function loadPrompts(): Prompt[] {
  try { return JSON.parse(localStorage.getItem("cc.prompts") || "[]") as Prompt[]; } catch { return []; }
}

/** One agent's moment: it asks to do something (Allow / Deny), needs you, or is done. */
export function buildAgent(a: { answer(e: AgentEv, allow: boolean): void; close(): void }): ViewHost & { show(e: AgentEv): void; current(): AgentEv | null } {
  const title = h("div", { class: "sg-title" });
  const sub = h("div", { class: "sg-sub" });
  const what = h("div", { class: "ag-what" });
  const acts = h("div", { class: "m-actions" });
  const el = h("div", { class: "view suggest agent" }, h("div", { class: "sg-text ag-text" }, title, sub, what, acts));
  let cur: AgentEv | null = null;
  let tick = 0;
  return {
    el,
    sync() {},
    current: () => cur,
    show(e) {
      cur = e;
      window.clearInterval(tick);
      const who = agentName(e.source);
      const where = [e.project, e.pcName].filter(Boolean).join(" · on ");
      sub.textContent = where;
      what.textContent = e.state === "approve" ? `${e.tool ? e.tool + ": " : ""}${e.text}` : e.text;
      what.hidden = !e.text;
      if (e.state === "approve") {
        title.textContent = `${who} asks to ${e.tool === "Bash" || e.tool === "PowerShell" ? "run a command" : e.tool === "Edit" || e.tool === "Write" || e.tool === "MultiEdit" ? "change a file" : "use " + (e.tool || "a tool")}`;
        const left = h("span", { class: "ag-left" });
        const end = Date.now() + 60_000;
        const count = () => { left.textContent = `${Math.max(0, Math.ceil((end - Date.now()) / 1000))} s`; };
        count();
        tick = window.setInterval(count, 1000);
        acts.replaceChildren(
          h("button", { class: "btn primary", text: "Allow", onclick: () => { window.clearInterval(tick); a.answer(e, true); } }),
          h("button", { class: "btn", text: "Deny", onclick: () => { window.clearInterval(tick); a.answer(e, false); } }),
          left);
      } else {
        title.textContent = e.state === "done" ? `${who} is done ✓` : `${who} needs you`;
        acts.replaceChildren(h("button", { class: "btn", text: "OK", onclick: () => a.close() }));
      }
    },
  };
}

const fmtTok = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : String(n));
const ago = (t: number) => {
  const s = Math.round((Date.now() - t) / 1000);
  return s < 60 ? "now" : s < 3600 ? `${Math.round(s / 60)} min` : `${Math.round(s / 3600)} h`;
};

/** AI agents: who is working where, today's usage, Ask Bob and your prompts. */
export function buildAi(a: {
  answer(e: AgentEv, allow: boolean): void;
  ask(q: string): Promise<string>;
  copy(text: string, what: string): void;
  usage(): Promise<{ claude: { input: number; output: number; cacheRead: number; cacheWrite: number; cost: number }; codex: { input: number; output: number } } | null>;
}): ViewHost & { refresh(): void; askNow(q: string): void } {
  const list = h("div", { class: "ai-list" });
  const use = h("div", { class: "ai-use" });
  const input = h("input", { class: "ai-in", placeholder: "Ask Bob anything…", maxLength: 8000 }) as HTMLInputElement;
  const out = h("div", { class: "ai-out", hidden: true });
  const prompts = h("div", { class: "ai-prompts" });
  let busy = false;
  const ask = async (q: string) => {
    if (!q.trim() || busy) return;
    busy = true;
    out.hidden = false;
    out.className = "ai-out thinking";
    out.textContent = "Bob is thinking…";
    try {
      const ans = await a.ask(q);
      out.className = "ai-out";
      out.replaceChildren(h("div", { class: "ai-ans", text: ans }), h("button", { class: "btn ai-copy", text: "Copy", onclick: () => a.copy(ans, "The answer") }));
    } catch (e) {
      out.className = "ai-out err";
      out.textContent = String(e);
    }
    busy = false;
  };
  input.onkeydown = (e) => { if (e.key === "Enter") { void ask(input.value); } };
  const el = h("div", { class: "view ai" },
    h("div", { class: "ai-col" }, h("div", { class: "ai-h", text: "Agents" }), list, use),
    h("div", { class: "ai-col" },
      h("div", { class: "ai-h", text: "Ask Bob" }),
      h("div", { class: "ai-ask" }, input, h("button", { class: "btn primary", text: "Ask", onclick: () => void ask(input.value) })),
      out,
      h("div", { class: "ai-h", text: "Prompts" }),
      prompts));
  const renderList = () => {
    const all = [...agents.values()].sort((x, y) => y.at - x.at).slice(0, 4);
    list.replaceChildren(...(all.length ? all.map((e) => h("div", { class: `ai-ag s-${e.state}` },
      h("i", { class: "ai-dot" }),
      h("div", { class: "ai-ag-t" },
        h("b", { text: `${agentName(e.source)}${e.project ? " · " + e.project : ""}` }),
        h("span", { text: `${STATE_WORDS[e.state] ?? e.state} · ${e.pcName} · ${ago(e.at)}` })),
      ...(e.state === "approve" ? [h("button", { class: "btn primary", text: "Allow", onclick: () => a.answer(e, true) }), h("button", { class: "btn", text: "Deny", onclick: () => a.answer(e, false) })] : [])))
      : [h("div", { class: "ai-empty", text: "No agents yet. Connect Claude Code or Codex in Settings → AI, and they show up here from any of your computers." })]));
    const ps = loadPrompts().filter((p) => !p.del);
    prompts.replaceChildren(...(ps.length ? ps.slice(0, 8).map((p) => h("button", { class: "ai-chip", text: p.title || p.text.slice(0, 24), title: p.text, onclick: () => a.copy(p.text, `“${p.title || "Prompt"}”`) }))
      : [h("span", { class: "ai-empty", text: "Save prompts in Settings → AI; they're here on all your computers." })]));
  };
  return {
    el,
    sync() {},
    refresh() {
      renderList();
      void a.usage().then((u) => {
        if (!u) return;
        const c = u.claude, x = u.codex;
        const parts: string[] = [];
        if (c.input + c.output + c.cacheRead + c.cacheWrite > 0) parts.push(`Claude Code ${fmtTok(c.input + c.output + c.cacheRead + c.cacheWrite)} tokens ≈ $${c.cost.toFixed(2)}`);
        if (x.input + x.output > 0) parts.push(`Codex ${fmtTok(x.input + x.output)} tokens`);
        use.textContent = parts.length ? `Last 24 h on this computer: ${parts.join(" · ")}` : "No AI usage on this computer in the last 24 h.";
        use.title = "Counted from Claude Code's and Codex's own logs. The cost is at API list prices; on a subscription you don't pay it per token.";
      });
    },
    askNow(q) {
      input.value = q.length > 300 ? q.slice(0, 300) + "…" : q;
      void ask(q);
    },
  };
}

// ------------------------------------------------------------------ all the lyrics

/** The whole song's words, the line being sung in the middle and lit up. */
export function buildLyrics(a: { back(): void }): ViewHost {
  const list = h("div", { class: "ly-list" });
  const msg = h("div", { class: "ly-msg" });
  const el = h("div", { class: "view lyrics" }, list, msg,
    h("button", { class: "btn ly-back", text: "Back", onclick: () => a.back() }));
  let built = "";
  let at = -2;
  let rows: HTMLElement[] = [];
  return {
    el,
    sync() {
      if (State.view !== "lyrics") return;
      const l = State.lyrics;
      const key = `${State.lyricsFor}|${Array.isArray(l) ? l.length : typeof l}`;
      if (key !== built) {
        built = key;
        at = -2;
        if (Array.isArray(l)) {
          rows = l.map((x) => h("div", { class: "ly-line", text: x.text || "♪" }));
          list.replaceChildren(...rows);
          msg.textContent = "";
        } else if (typeof l === "string") {
          rows = [];
          list.replaceChildren(...l.split("\n").map((x) => h("div", { class: "ly-line plain", text: x })));
          msg.textContent = "";
        } else {
          rows = [];
          list.replaceChildren();
          msg.textContent = State.settings.lyrics ? "Looking up the lyrics…" : "Turn lyrics on (the Lyrics button) to see the words.";
        }
      }
      if (!rows.length || !Array.isArray(l)) return;
      const ms = mediaPosition();
      let i = -1;
      for (let k = 0; k < l.length && l[k].ms <= ms; k++) i = k;
      if (i === at) return;
      at = i;
      rows.forEach((r, k) => { r.classList.toggle("on", k === i); r.classList.toggle("past", k < i); });
      const cur = rows[Math.max(0, i)];
      list.scrollTo({ top: cur.offsetTop - list.clientHeight / 2 + cur.clientHeight / 2, behavior: "smooth" });
    },
  };
}

// ------------------------------------------------------------------ listening stats

export interface ListenDay { t: number; a: Record<string, number>; s: Record<string, number> }
export function loadListen(): Record<string, ListenDay> {
  try { return JSON.parse(localStorage.getItem("cc.listen") || "{}") as Record<string, ListenDay>; } catch { return {}; }
}
export function dayKey(d = new Date()): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
/** The last 7 days added up: total seconds, top artists, top songs. */
export function listenWeek(): { secs: number; artists: [string, number][]; songs: [string, number][] } {
  const days = loadListen();
  const a = new Map<string, number>(), s = new Map<string, number>();
  let secs = 0;
  for (let k = 0; k < 7; k++) {
    const d = days[dayKey(new Date(Date.now() - k * 864e5))];
    if (!d) continue;
    secs += d.t;
    for (const [n, v] of Object.entries(d.a)) a.set(n, (a.get(n) ?? 0) + v);
    for (const [n, v] of Object.entries(d.s)) s.set(n, (s.get(n) ?? 0) + v);
  }
  const top = (m: Map<string, number>) => [...m.entries()].sort((x, y) => y[1] - x[1]).slice(0, 3);
  return { secs, artists: top(a), songs: top(s) };
}
export const hoursText = (secs: number) => {
  const m = Math.round(secs / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h${m % 60 ? ` ${m % 60} min` : ""}`;
};

export function buildStats(a: { back(): void }): ViewHost {
  const big = h("div", { class: "st-big" });
  const cols = h("div", { class: "st-cols" });
  const el = h("div", { class: "view stats" }, h("div", { class: "st-text" }, big, cols),
    h("button", { class: "btn ly-back", text: "Back", onclick: () => a.back() }));
  let was = false;
  return {
    el,
    sync() {
      const open = State.view === "stats";
      if (open && !was) {
        const w = listenWeek();
        big.textContent = w.secs ? `${hoursText(w.secs)} of music this week 🎧` : "No music this week yet 🎧";
        const col = (t: string, list: [string, number][]) => h("div", { class: "st-col" }, h("div", { class: "st-h", text: t }),
          ...(list.length ? list.map(([n, v], i) => h("div", { class: "st-row" }, h("b", { text: `${i + 1}` }), h("span", { text: n }), h("small", { text: hoursText(v) })))
            : [h("div", { class: "st-row" }, h("span", { text: "—" }))]));
        cols.replaceChildren(col("Top artists", w.artists), col("Top songs", w.songs));
      }
      was = open;
    },
  };
}
