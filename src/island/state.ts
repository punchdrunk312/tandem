// Island UI state: what the island is showing plus the latest Tandem snapshot.

import type { BotStateName, IslandMode, IslandViewName } from "../core/layout";
import type { Snapshot } from "../types";

export interface IslandSettings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
}

const DEFAULTS: IslandSettings = { soundEnabled: true, soundVolume: 0.12, autoCloseInterval: 15 };
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
