// Island geometry (from Coucou's layout, trimmed to Tandem's views). Logical px.

export type IslandMode = "hidden" | "compact" | "expanded";
export type IslandViewName = "home" | "drop" | "note" | "greeting" | "moment" | "devices" | "focus" | "update" | "media" | "notes" | "nowplaying" | "recipients" | "video" | "charge";

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
  /** Narrower than the usual expanded bar (the song strip). */
  width?: number;
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
  // A moment: the ring on the left, the words in the middle, Bob reacting on the right.
  moment: { height: 104, botX: 586, botY: 54, botDiameter: 46 },
  // The device cards fill the bar; Bob steps aside.
  devices: { height: 232, botX: 0, botY: 0, botDiameter: 0 },
  // The focus timer: Bob on the left keeping time, the clock in the middle.
  focus: { height: 162, botX: 68, botY: 98, botDiameter: 58 },
  // Now playing: Bob listens on the left, the song, its progress, buttons and a lyric line.
  media: { height: 176, botX: 68, botY: 100, botDiameter: 58 },
  // Who gets the dropped files: a chip per connected computer (Bob steps aside).
  recipients: { height: 214, botX: 0, botY: 0, botDiameter: 0 },
  // A song just started: a slim strip with Bob, the song and two buttons (no header).
  nowplaying: { height: 56, botX: 30, botY: 26, botDiameter: 28, width: 430 },
  // A YouTube video whose browser was minimised, playing on in the bar (Bob steps aside).
  video: { height: 250, botX: 0, botY: 0, botDiameter: 0, width: 428 },
  // This computer's charger, like a phone's: a slim wide pill, the battery left, the level right.
  charge: { height: 40, botX: 0, botY: 0, botDiameter: 0, width: 400 },
  // Notes: a reminder that is due, or the next note and a couple more, or writing one.
  notes: { height: 196, botX: 68, botY: 104, botDiameter: 58 },
  // A new version: Bob holds up the news, the words and two buttons beside him.
  update: { height: 128, botX: 62, botY: 70, botDiameter: 52 },
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
      return { w: VIEW_LAYOUTS[view].width ?? EXPANDED_W, h: VIEW_LAYOUTS[view].height + extra };
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
      if (l.botDiameter === 0) return { cx: 40, cy: 16, diameter: 20, opacity: 0 };
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
