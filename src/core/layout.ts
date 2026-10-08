// Island geometry (from Coucou's layout, trimmed to Tandem's views). Logical px.

export type IslandMode = "hidden" | "compact" | "expanded";
export type IslandViewName = "home" | "drop" | "note" | "greeting";

export type BotStateName =
  | "idle"
  | "working"
  | "thinking"
  | "searching"
  | "approval"
  | "question"
  | "error"
  | "finished"
  | "ratelimit"
  | "sleeping"
  | "dizzy";

export type BotEmoteName = "love" | "surprised" | "proud" | "wink" | "yawn" | "happy" | "annoyed";

export interface ViewLayout {
  height: number;
  botX: number;
  botY: number;
  botDiameter: number;
}

// The window is a fixed 720×320; the island is drawn inside it, glued to the top
// edge and horizontally centred.
export const PANEL_W = 720;
export const PANEL_H = 320;

export const NOTCH_W = 184;
export const NOTCH_H = 32;
export const COMPACT_W = 288;
export const EXPANDED_W = 640;

export const ROUNDED_CORNER = 14;
export const EXPANDED_CORNER = 22;

export const VIEW_LAYOUTS: Record<IslandViewName, ViewLayout> = {
  home: { height: 164, botX: 68, botY: 102, botDiameter: 58 },
  drop: { height: 164, botX: 130, botY: 102, botDiameter: 66 },
  note: { height: 120, botX: 62, botY: 76, botDiameter: 50 },
  greeting: { height: 150, botX: 320, botY: 90, botDiameter: 0 },
};

/** Height added to the home view per visible transfer row. */
export const TRANSFER_ROW = 40;
export const MAX_TRANSFER_ROWS = 3;

export function islandSize(mode: IslandMode, view: IslandViewName, transferRows = 0): { w: number; h: number } {
  switch (mode) {
    case "hidden":
      // No notch to hide inside on a PC: retract to zero height at the top edge.
      return { w: NOTCH_W, h: 0 };
    case "compact":
      return { w: COMPACT_W, h: NOTCH_H };
    case "expanded": {
      const extra = view === "home" ? Math.min(MAX_TRANSFER_ROWS, transferRows) * TRANSFER_ROW : 0;
      return { w: EXPANDED_W, h: VIEW_LAYOUTS[view].height + extra };
    }
  }
}

export interface BotPlacement {
  cx: number;
  cy: number;
  diameter: number;
  opacity: number;
}

export function botPosition(mode: IslandMode, view: IslandViewName): BotPlacement {
  switch (mode) {
    case "hidden":
      return { cx: 46, cy: 16, diameter: 6, opacity: 0 };
    case "compact":
      return { cx: 40, cy: 16, diameter: 20, opacity: 1 };
    case "expanded": {
      const l = VIEW_LAYOUTS[view];
      return { cx: l.botX, cy: l.botY, diameter: l.botDiameter, opacity: 1 };
    }
  }
}

export function botGlowColor(s: BotStateName): string {
  switch (s) {
    case "working": return "#3B9EFF";
    case "thinking": return "#A78BFA";
    case "searching": return "#6366F1";
    case "approval": return "#F5A524";
    case "question": return "#22D3EE";
    case "error": return "#F4505E";
    case "finished": return "#34D399";
    case "ratelimit": return "#F59E0B";
    default: return "#FFFFFF";
  }
}

export function botGlowOpacity(s: BotStateName): number {
  switch (s) {
    case "idle":
    case "sleeping":
      return 0.15;
    case "dizzy":
      return 0;
    default:
      return 0.65;
  }
}
