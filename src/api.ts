import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Snapshot } from "./types";
import * as mock from "./mock";

const inTauri = "__TAURI_INTERNALS__" in window;

export function call<T = void>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return inTauri ? invoke<T>(cmd, args) : (mock.call(cmd, args) as Promise<T>);
}

export function onSnapshot(cb: (s: Snapshot) => void) {
  if (inTauri) void listen<Snapshot>("snapshot", (e) => cb(e.payload));
  else mock.onSnapshot(cb);
}

export function onFileDrop(onPaths: (p: string[]) => void, onHover: (over: boolean) => void) {
  if (!inTauri) return;
  void getCurrentWebview().onDragDropEvent((e) => {
    const t = e.payload.type;
    if (t === "enter" || t === "over") onHover(true);
    else if (t === "leave") onHover(false);
    else if (t === "drop") {
      onHover(false);
      onPaths(e.payload.paths);
    }
  });
}
