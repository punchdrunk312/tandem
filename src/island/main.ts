// Island entry: Bob at the top of the screen.

import "./island.css";
import { IS_TAURI, call, fire, onEvent, onSnapshot } from "../bridge";
import { Sound } from "../core/sound";
import type { Snapshot } from "../types";
import { Island, bootSnapshot } from "./island";
import { SETTINGS_KEY, State } from "./state";
import { NOTES_KEY } from "../core/notes";

async function main() {
  const root = document.getElementById("root");
  if (!root) return;
  if (!IS_TAURI) document.documentElement.classList.add("dev");

  void Sound.preload();
  const island = new Island(root);
  island.applySettings();

  const safe = async (what: string, f: () => Promise<unknown> | unknown) => {
    try {
      await f();
    } catch (err) {
      fire("log_js", { msg: `${what}: ${String(err)}` });
    }
  };
  window.addEventListener("error", (e) => fire("log_js", { msg: `error: ${e.message}` }));
  window.addEventListener("unhandledrejection", (e) => fire("log_js", { msg: `rejection: ${String(e.reason)}` }));

  await safe("snapshot listener", () => onSnapshot((s: Snapshot) => island.applySnapshot(s)));
  await safe("cursor listener", () => onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y)));
  await safe("hotkey listener", () => onEvent<"bar" | "pill">("hotkey", (what) => island.hotkey(what)));
  await safe("media listener", () => onEvent<import("./state").NowPlaying | null>("media", (m) => island.onMedia(m)));
  await safe("note listener", () => onEvent<string>("note", (msg) => island.note(msg)));
  await safe("quiet listener", () => onEvent<boolean>("quiet", (q) => island.setQuiet(q)));
  await safe("drag-hint listener", () => onEvent<boolean>("drag-hint", (on) => island.dragHint(on)));
  await safe("screen listener", () => onEvent<null>("screen-changed", () => fire("reposition")));
  // Liquid Glass: pictures of what is behind the bar, raw, over a binary channel.
  await safe("glass channel", async () => {
    if (!IS_TAURI) return;
    const { Channel } = await import("@tauri-apps/api/core");
    const ch = new Channel<ArrayBuffer>();
    ch.onmessage = (buf) => {
      if (!(buf instanceof ArrayBuffer) || buf.byteLength < 24) return;
      const v = new DataView(buf);
      const pw = v.getUint32(16, true), ph = v.getUint32(20, true);
      if (buf.byteLength < 24 + pw * ph * 4) return;
      island.setBackdropFrame({
        x: v.getFloat32(0, true),
        y: v.getFloat32(4, true),
        w: v.getFloat32(8, true),
        h: v.getFloat32(12, true),
        pw,
        ph,
        rgba: new Uint8ClampedArray(buf, 24, pw * ph * 4),
      });
    };
    await call("glass_stream", { channel: ch });
  });
  await safe("update listener", () =>
    onEvent<{ current: string; version: string | null; notes: string }>("update", (u) => island.updateReady(u)));
  await safe("boot", () => bootSnapshot(island));
  // An update found before the bar was listening.
  void call<{ current: string; version: string | null; notes: string }>("update_status")
    .then((u) => u && island.updateReady(u))
    .catch(() => {});
  // The pairing/settings window edits the same settings entry.
  window.addEventListener("storage", (e) => {
    if (e.key === NOTES_KEY) island.reloadNotes();
    if (e.key === SETTINGS_KEY) {
      State.reloadSettings();
      island.applySettings();
    }
  });
  island.launch();
  fire("log_js", { msg: "island launched" });

  if (!IS_TAURI) {
    document.addEventListener("click", () => Sound.resume(), { once: true });
    // A pretend desktop behind the bar, photographed the way the app does it.
    const { startDevDesk } = await import("./devdesk");
    startDevDesk(island);
  }
}

void main();
