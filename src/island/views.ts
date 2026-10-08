// The island's views: header, home (status + actions + transfers), drop, note.
// Everything that comes from the network goes through text nodes.

import { fire } from "../bridge";
import type { Snapshot } from "../types";
import { h, svg } from "./dom";
import { State } from "./state";

export interface ViewHost {
  el: HTMLElement;
  sync(): void;
}

export interface ViewActions {
  collapse(): void;
  toggleSound(): void;
  blip(): void;
}

const ICON = {
  close: "M6 6l12 12M18 6L6 18",
  sliders: "M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6",
  speaker: "M11 5L6 9H3v6h3l5 4V5zM15.5 8.5a5 5 0 010 7M18 6a8.5 8.5 0 010 12",
  speakerOff: "M11 5L6 9H3v6h3l5 4V5zM16 9l5 6M21 9l-5 6",
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
  const title = h("span", { class: "hd-title", text: "Tandem" });
  const sound = h("button", { class: "icon-btn", title: "Sound", "aria-label": "Toggle sound", onclick: () => a.toggleSound() });
  const settings = h("button", { class: "icon-btn", title: "Pair & settings", "aria-label": "Open settings", onclick: () => { a.blip(); fire("open_main_window"); } }, svg(ICON.sliders, 15, { stroke: 1.8 }));
  const close = h("button", { class: "icon-btn", title: "Close", "aria-label": "Close", onclick: () => a.collapse() }, svg(ICON.close, 15, { stroke: 1.8 }));
  const el = h("div", { id: "header" }, dot, title, h("span", { class: "spacer" }), sound, settings, close);
  return {
    el,
    sync() {
      const live = State.snap?.control && State.snap.control !== "offline";
      dot.className = `hd-dot${live ? " on" : ""}`;
      sound.replaceChildren(svg(State.settings.soundEnabled ? ICON.speaker : ICON.speakerOff, 15, { stroke: 1.8 }));
    },
  };
}

// ------------------------------------------------------------------ home

export function homeText(s: Snapshot | null): { title: string; sub: string } {
  if (!s) return { title: "Starting…", sub: "" };
  const peer = s.connected?.name ?? "";
  const side = s.paired.find((p) => p.connected)?.side.toLowerCase() ?? "right";
  switch (s.control) {
    case "local":
      return { title: `Connected to ${peer}`, sub: `Push your cursor through the ${side} edge — or drop a file on me to send it.` };
    case "remote":
      return { title: `Controlling ${peer}`, sub: "Your mouse and keyboard are over there. Push back through the edge, or press Scroll Lock." };
    case "controlled":
      return { title: `${peer} is driving`, sub: "Move to the edge to take your mouse back." };
    default: {
      const near = s.nearby.find((n) => n.paired) ?? s.paired.find((p) => p.nearby);
      if (near) return { title: `${near.name} is nearby`, sub: "Connect to share your mouse, keyboard and clipboard." };
      if (s.nearby.length) return { title: "A computer wants a friend", sub: `Open settings and type the code shown on ${s.nearby[0].name}.` };
      return { title: "Not connected", sub: s.paired.length ? "Waiting for your other computer to show up…" : "Run Tandem on your other computer, then pair them in settings." };
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
      keyed(actions, `${s?.control}|${s?.connected?.id}|${near?.id}|${s?.nearby.length}`, () => {
        if (!s) return [];
        if (s.connected) {
          return [btn(`Jump to ${s.connected.name}`, () => fire("engage"), "primary"), btn("Disconnect", () => fire("disconnect"))];
        }
        const out: Node[] = [];
        if (near) out.push(btn("Connect", () => fire("connect_known", { id: near.id }), "primary"));
        out.push(btn(s.paired.length ? "Settings…" : "Pair a computer…", () => fire("open_main_window"), near ? "" : "primary"));
        return out;
      });

      const peer = s?.paired.find((p) => p.connected);
      keyed(side, `${peer?.id}|${peer?.side}`, () => {
        if (!peer) return [];
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
      keyed(xfers, rows.map((r) => `${r.direction}${r.id}${r.state}${r.name}${r.message}`).join(","), () =>
        rows.map((r) =>
          h("div", { class: `xfer ${r.state}`, "data-id": `${r.direction}${r.id}` },
            h("span", { class: "x-dir", text: r.direction === "out" ? "↑" : "↓" }),
            h("div", { class: "x-body" },
              h("div", { class: "x-top" }, h("span", { class: "x-name", text: r.name }), h("span", { class: "x-state", text: "" })),
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

export function buildDrop(): ViewHost {
  const big = h("div", { class: "d-title" });
  const small = h("div", { class: "d-sub" });
  const el = h("div", { class: "view drop" }, h("div", { class: "d-text" }, big, small));
  return {
    el,
    sync() {
      const peer = State.connected?.name;
      big.textContent = peer ? `Drop to send to ${peer}` : "Connect to a computer first";
      small.textContent = peer ? "Files and folders land in its Downloads\\Tandem folder." : "Open settings to pair, then drop again.";
      el.classList.toggle("bad", !peer);
    },
  };
}

export function buildNote(): ViewHost {
  const msg = h("div", { class: "n-msg" });
  const el = h("div", { class: "view note" }, msg);
  return { el, sync: () => (msg.textContent = State.notice) };
}
