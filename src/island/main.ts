// Island entry: Mochi at the top of the screen.

import "./island.css";
import { IS_TAURI, fire, onEvent, onSnapshot } from "../bridge";
import { Sound } from "../core/sound";
import type { Snapshot } from "../types";
import { Island, bootSnapshot } from "./island";

async function main() {
  const root = document.getElementById("root");
  if (!root) return;
  if (!IS_TAURI) document.documentElement.classList.add("dev");

  void Sound.preload();
  const island = new Island(root);
  island.applySettings();

  onSnapshot((s: Snapshot) => island.applySnapshot(s));
  await onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y));
  await onEvent<string>("tray", (what) => {
    if (what === "open") island.alert("home");
  });
  await onEvent<null>("screen-changed", () => fire("reposition"));

  await bootSnapshot(island);
  island.launch();

  if (!IS_TAURI) document.addEventListener("click", () => Sound.resume(), { once: true });
}

void main();
