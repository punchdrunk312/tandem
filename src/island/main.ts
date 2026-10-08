// Island entry: Mochi at the top of the screen.

import "./island.css";
import { IS_TAURI, fire, onEvent, onSnapshot } from "../bridge";
import { Sound } from "../core/sound";
import type { Snapshot } from "../types";
import { Island, bootSnapshot } from "./island";
import { SETTINGS_KEY, State } from "./state";

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
  await safe("summon listener", () => onEvent<null>("summon", () => island.summon()));
  await safe("wake listener", () => onEvent<null>("wake", () => island.wake()));
  await safe("quiet listener", () => onEvent<boolean>("quiet", (q) => island.setQuiet(q)));
  await safe("drag-hint listener", () => onEvent<boolean>("drag-hint", (on) => island.dragHint(on)));
  await safe("screen listener", () => onEvent<null>("screen-changed", () => fire("reposition")));
  await safe("boot", () => bootSnapshot(island));
  // The pairing/settings window edits the same settings entry.
  window.addEventListener("storage", (e) => {
    if (e.key === SETTINGS_KEY) {
      State.reloadSettings();
      island.applySettings();
    }
  });
  island.launch();
  fire("log_js", { msg: "island launched" });

  if (!IS_TAURI) document.addEventListener("click", () => Sound.resume(), { once: true });
}

void main();
