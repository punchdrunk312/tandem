// Island UI state: what the island is showing plus the latest Tandem snapshot.

import type { BotStateName, IslandMode, IslandViewName } from "../core/layout";
import type { Snapshot } from "../types";

export interface IslandSettings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  /** Bob dresses for the season and the special days. */
  seasons: boolean;
  /** The device moments (plugged in, low, full, a computer joining…). */
  moments: boolean;
  /** A soft whoosh when the cursor crosses to another computer. */
  crossWhoosh: boolean;
  /** "MM-DD", or empty. */
  birthday: string;
  /** Calm Bob: no seasons drifting, quieter moments. */
  calm: boolean;
  /** The look: frosted "glass" (default) or the classic black bar. */
  theme: "glass" | "dark";
  /** Bob shows one tip a day about something CubeCursor can do. */
  tips: boolean;
  /** Minutes the focus timer starts with. */
  focusMinutes: number;
  /** A card with the battery when earbuds (or a headset, a mouse) connect. */
  accCards: boolean;
}

/** A focus timer that is running: when it ends and how long it was (ms). */
export interface Focus {
  endsAt: number;
  total: number;
}

const FOCUS_KEY = "cc.focus";

export function loadFocus(): Focus | null {
  try {
    const f = JSON.parse(localStorage.getItem(FOCUS_KEY) ?? "null") as Focus | null;
    return f && f.endsAt > Date.now() ? f : null;
  } catch {
    return null;
  }
}

export function saveFocus(f: Focus | null) {
  try {
    if (f) localStorage.setItem(FOCUS_KEY, JSON.stringify(f));
    else localStorage.removeItem(FOCUS_KEY);
  } catch {
    /* fine: it just won't survive a restart */
  }
}

const DEFAULTS: IslandSettings = {
  soundEnabled: true, soundVolume: 0.12, autoCloseInterval: 15,
  seasons: true, moments: true, crossWhoosh: false, birthday: "", calm: false,
  theme: "glass",
  tips: true,
  focusMinutes: 25,
  accCards: true,
};
export const SETTINGS_KEY = "tandem.island";
const KEY = SETTINGS_KEY;

function load(): IslandSettings {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return { ...DEFAULTS };
  }
}

type Listener = () => void;

class IslandState {
  mode: IslandMode = "hidden";
  view: IslandViewName = "home";

  snap: Snapshot | null = null;

  /** Cursor in window-logical px, and relative to the island's top-left. */
  mouse = { x: 0, y: 0 };
  mouseInIsland = { x: 0, y: 0 };

  fileDragOver = false;
  /** Which computer's drop target the dragged files are over (several computers only). */
  dropTarget: string | null = null;
  /** Dropped files being copied to the staging folder before the transfer starts. */
  staging = 0;
  isPinned = false;
  lastActivity = performance.now();

  /** Slaps → dizzy overrides everything for a moment. */
  stateOverride: BotStateName | null = null;
  /** Brief reaction to a finished / failed transfer. */
  flash: BotStateName | null = null;
  notice = "";
  /** The focus timer, while it runs (it survives a restart). */
  focus: Focus | null = loadFocus();
  /** The moment on show (see moments.ts). */
  moment: import("./moments").Moment | null = null;
  /** A special day's line for the header ("Merry Christmas!"), or null. */
  greeting: string | null = null;

  settings: IslandSettings = load();

  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  notify() {
    for (const fn of this.listeners) fn();
  }

  /** The panel window edits the same localStorage entry: pick its changes up. */
  reloadSettings() {
    this.settings = load();
  }

  saveSettings() {
    try {
      localStorage.setItem(KEY, JSON.stringify(this.settings));
    } catch {
      /* private mode etc. — the island works without it */
    }
  }

  get connected() {
    return this.snap?.connected ?? null;
  }

  /** Every connected computer. */
  get connectedPeers() {
    return (this.snap?.paired ?? []).filter((p) => p.connected);
  }

  /** Name of the computer in control (or being controlled), falling back to any connected one. */
  get controlName(): string {
    const s = this.snap;
    return s?.paired.find((p) => p.id === s.control_peer)?.name ?? s?.connected?.name ?? "";
  }

  get activeTransfers() {
    return (this.snap?.transfers ?? []).filter((t) => t.state === "active");
  }

  /** How many transfer rows the home view shows (active first, then recent). */
  get transferRows(): number {
    return Math.min(3, this.snap?.transfers.length ?? 0);
  }

  get effectiveState(): BotStateName {
    if (this.stateOverride) return this.stateOverride;
    const s = this.snap;
    if (!s) return "sleeping";
    if (this.flash) return this.flash;
    if (this.activeTransfers.length) return "working";
    switch (s.control) {
      case "remote":
        return "working";
      case "controlled":
        return "thinking";
      case "local":
        return "idle";
      default:
        return !s.paired.length && s.nearby.some((n) => !n.paired) ? "question" : "sleeping";
    }
  }

  defaultView(): IslandViewName {
    return "home";
  }
}

export const State = new IslandState();
