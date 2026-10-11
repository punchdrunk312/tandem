export type Side = "left" | "right" | "top" | "bottom";
export type Control = "offline" | "local" | "remote" | "controlled";

export interface Snapshot {
  device_id: string;
  device_name: string;
  pairing_code: string;
  tcp_port: number;
  control: Control;
  /** The computer the cursor is on / that is driving this one. */
  control_peer: string | null;
  /** One connected computer (the one in control, else the first); see `paired[].connected`. */
  connected: { id: string; name: string } | null;
  paired: { id: string; name: string; side: "Left" | "Right" | "Top" | "Bottom"; connected: boolean; nearby: boolean }[];
  nearby: { id: string; name: string; addr: string; port: number; paired: boolean }[];
  transfers: {
    id: number;
    name: string;
    direction: "in" | "out";
    bytes: number;
    total: number;
    state: "active" | "done" | "error";
    message: string;
    path: string | null;
    /** The other computer's name. */
    peer: string;
  }[];
  clipboard_sync: boolean;
  game_mode: boolean;
  switch_delay_ms: number;
  error: string | null;
  /** A short message that clears itself, e.g. why control came back. */
  notice: string | null;
}

export interface ElevState {
  elevated: boolean;
  autostart: boolean;
}

export interface UiOptions {
  enabled: boolean;
  hide_fullscreen: boolean;
  monitor_cursor: boolean;
}
