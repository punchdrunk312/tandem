// Thin layer over Tauri used by both windows (island + panel). Outside Tauri
// (plain browser, `npm run dev`) calls go to a mock so the UI can be developed
// and screenshotted without the Rust side.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Snapshot } from "./types";
import * as mock from "./mock";

export const IS_TAURI = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function call<T = void>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) return (await mock.call(cmd, args)) as T;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[tandem] ${cmd} failed`, err);
    throw err;
  }
}

/** Fire-and-forget variant for UI chrome that must never throw. */
export function fire(cmd: string, args?: Record<string, unknown>) {
  void call(cmd, args).catch(() => {});
}

export function onSnapshot(cb: (s: Snapshot) => void) {
  if (IS_TAURI) void listen<Snapshot>("snapshot", (e) => cb(e.payload));
  else mock.onSnapshot(cb);
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (IS_TAURI) await listen<T>(name, (e) => handler(e.payload));
  else mock.onEvent(name, handler as (p: unknown) => void);
}

export interface DragDropPayload {
  type: "enter" | "over" | "leave" | "drop";
  paths?: string[];
}

export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (IS_TAURI) {
    await getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      handler({ type: p.type, paths: "paths" in p ? p.paths : undefined });
    });
  } else {
    mock.onDragDrop(handler);
  }
}
