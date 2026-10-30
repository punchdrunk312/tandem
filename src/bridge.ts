// Thin layer over Tauri used by both windows (island + panel). Outside Tauri
// (plain browser, `npm run dev`) calls go to a mock so the UI can be developed
// and screenshotted without the Rust side.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

/** Files dropped on a window: HTML5 drop → copied into a staging folder → sent. */
/** How the files are prepared before sending (Shift-drop menu). */
export type DropAction = "asis" | "zip" | "png" | "jpg";

export interface Dropped {
  /** Top-level items (files and folders). */
  count: number;
  /** Send to one computer; without an id: to the one being controlled, or to all.
   * `action` transforms the files first (zip them, convert images). */
  send(peerId?: string, action?: DropAction): Promise<void>;
}

export interface DragDropPayload {
  type: "enter" | "over" | "leave" | "drop";
  drop?: Dropped;
  /** Id of the computer under the pointer (an element with `data-peer`), if any. */
  target?: string;
  /** Shift was held at drop: offer the prepare-before-send menu. */
  shift?: boolean;
}

const CHUNK = 4 * 1024 * 1024;
const hex = (str: string) => Array.from(new TextEncoder().encode(str), (b) => b.toString(16).padStart(2, "0")).join("");

interface Item {
  rel: string;
  file: File;
}

async function walk(entry: FileSystemEntry, prefix: string, out: Item[]): Promise<void> {
  if (entry.isFile) {
    const file = await new Promise<File>((res, rej) => (entry as FileSystemFileEntry).file(res, rej));
    out.push({ rel: prefix + entry.name, file });
  } else if (entry.isDirectory) {
    const reader = (entry as FileSystemDirectoryEntry).createReader();
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((res, rej) => reader.readEntries(res, rej));
      if (!batch.length) break;
      for (const e of batch) await walk(e, `${prefix}${entry.name}/`, out);
    }
  }
}

async function stageAndSend(tops: { name: string; entry: FileSystemEntry | null; file: File | null }[], peerId?: string, action?: string) {
  const items: Item[] = [];
  for (const t of tops) {
    if (t.entry) await walk(t.entry, "", items);
    else if (t.file) items.push({ rel: t.file.name, file: t.file });
  }
  if (!items.length) return;
  const id = await call<string>("stage_begin");
  for (const { rel, file } of items) {
    let off = 0;
    do {
      const buf = new Uint8Array(await file.slice(off, off + CHUNK).arrayBuffer());
      if (IS_TAURI) {
        await invoke("stage_chunk", buf, { headers: { "x-session": id, "x-path": hex(rel), "x-offset": String(off) } });
      }
      off += buf.length;
    } while (off < file.size);
  }
  await call("stage_finish", { id, names: tops.map((t) => t.name), peerId, action: action && action !== "asis" ? action : null });
}

/** Whole-window drop handling for both windows. Works in a plain browser too. */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  let depth = 0;
  const peerAt = (e: Event): string | undefined => {
    const t = e.target;
    return t instanceof Element ? t.closest<HTMLElement>("[data-peer]")?.dataset.peer : undefined;
  };
  const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes("Files");
  document.addEventListener("dragenter", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth++;
    handler({ type: "enter", target: peerAt(e) });
  });
  document.addEventListener("dragover", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    handler({ type: "over", target: peerAt(e) });
  });
  document.addEventListener("dragleave", (e) => {
    if (!hasFiles(e)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0) handler({ type: "leave" });
  });
  document.addEventListener("drop", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth = 0;
    // Entries must be read synchronously, inside the event.
    const tops: { name: string; entry: FileSystemEntry | null; file: File | null }[] = [];
    const items = Array.from(e.dataTransfer?.items ?? []);
    for (const it of items) {
      if (it.kind !== "file") continue;
      const entry = it.webkitGetAsEntry?.() ?? null;
      const file = it.getAsFile();
      const name = entry?.name ?? file?.name;
      if (name) tops.push({ name, entry, file });
    }
    handler({ type: "drop", target: peerAt(e), shift: e.shiftKey, drop: { count: tops.length, send: (peerId, action) => stageAndSend(tops, peerId, action) } });
  });
  // Dropping on the page must never navigate away to the file.
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("drop", (e) => e.preventDefault());
}
