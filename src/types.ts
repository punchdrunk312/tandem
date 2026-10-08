export type Side = "left" | "right" | "top" | "bottom";
export type Control = "offline" | "local" | "remote" | "controlled";

export interface Snapshot {
  device_id: string;
  device_name: string;
  pairing_code: string;
  tcp_port: number;
  control: Control;
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
  }[];
  clipboard_sync: boolean;
  game_mode: boolean;
  switch_delay_ms: number;
  error: string | null;
}

export interface UiOptions {
  enabled: boolean;
  hide_fullscreen: boolean;
  monitor_cursor: boolean;
}
