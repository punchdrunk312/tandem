export type Side = "left" | "right" | "top" | "bottom" | "topleft" | "topright" | "bottomleft" | "bottomright";
/** As the Rust side names them. */
export type SideName = "Left" | "Right" | "Top" | "Bottom" | "TopLeft" | "TopRight" | "BottomLeft" | "BottomRight";
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
  paired: PairedPeer[];
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
  /** Carry files that are being dragged across the screen edge. */
  drag_files: boolean;
  move_apps: boolean;
  /** Second-monitor mode: windows dragged across stay here and show there. */
  monitor_mode: boolean;
  /** Files being carried to this computer right now (shown under the cursor). */
  carry: { name: string; count: number; folder: boolean } | null;
  switch_delay_ms: number;
  error: string | null;
  /** A short message that clears itself, e.g. why control came back. */
  notice: string | null;
  /** The last move that can still be undone, for two minutes. */
  undo?: string | null;
  /** Second-monitor mode: windows shown across (ours there, theirs here). */
  windows_away?: number;
  /** How this computer is doing. */
  me?: DeviceInfo;
  /** This computer's colour, "#rrggbb". */
  my_color?: string;
}

export interface PairedPeer {
  id: string;
  name: string;
  side: SideName;
  connected: boolean;
  nearby: boolean;
  /** How it is doing (while connected). */
  device?: DeviceInfo | null;
  /** Round trip in ms. */
  ping_ms?: number | null;
  /** Its colour, "#rrggbb". */
  color?: string;
  /** It can be woken over the network. */
  wakeable?: boolean;
}

export interface DeviceInfo {
  /** Percent; null without a battery (a desktop). */
  battery: number | null;
  plugged: boolean;
  charging: boolean;
  mins_left: number | null;
  mins_to_full: number | null;
  saver: boolean;
  cpu: number;
  mem: number;
  disk_free_gb: number;
  disk_total_gb: number;
  uptime_min: number;
  accessories: { name: string; battery: number }[];
  mac: string | null;
}

export interface ElevState {
  elevated: boolean;
  autostart: boolean;
}

export interface UiOptions {
  enabled: boolean;
  hide_fullscreen: boolean;
  monitor_cursor: boolean;
  /** Automatic error reports; null until asked. */
  error_reports?: boolean | null;
}
