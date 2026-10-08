// Edge drop strip: appears while a file is dragged towards the other computer.
import "./edge.css";
import { IS_TAURI, call, onDragDrop, onEvent, onSnapshot } from "../bridge";
import type { Snapshot } from "../types";

const strip = document.getElementById("strip")!;
const inner = document.getElementById("inner")!;
if (!IS_TAURI) document.documentElement.classList.add("dev");

const icon = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 16V4m0 0L7 9m5-5 5 5"/><path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3"/></svg>`;
let peer = "the other computer";

function paint() {
  inner.replaceChildren();
  const ico = document.createElement("div");
  ico.innerHTML = icon; // static, trusted markup
  const label = document.createElement("span");
  label.textContent = `Drop to send to ${peer}`; // computer names are network input: textContent only
  inner.append(ico, label);
}
paint();

onSnapshot((s: Snapshot) => {
  peer = s.connected?.name ?? "the other computer";
  paint();
});
void call<Snapshot>("snapshot").then((s) => {
  if (s?.connected) {
    peer = s.connected.name;
    paint();
  }
}).catch(() => {});

void onEvent<string>("edge-side", (side) => {
  strip.dataset.side = side;
  strip.classList.remove("over");
});

void onDragDrop((e) => {
  if (e.type === "enter" || e.type === "over") strip.classList.add("over");
  else if (e.type === "leave") strip.classList.remove("over");
  else if (e.type === "drop") {
    strip.classList.remove("over");
    if (e.drop?.count) void e.drop.send().catch((err) => console.error("send failed", err));
  }
});
