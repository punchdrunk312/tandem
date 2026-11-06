// The floating video window: move it by the picture, resize it by its edges, see through
// it with the slider. Where it sits, its size and its see-through level are remembered.
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { availableMonitors, getCurrentWindow, PhysicalPosition, PhysicalSize } from "@tauri-apps/api/window";
import "./video.css";

interface Info { id: string; title: string; ms: number; peer: string | null; peerName: string | null }

const win = getCurrentWindow();
const KEY = "cc.videoWindow";
const SEE = "cc.videoSee";
const VOL = "cc.videoVol";
const stored = (k: string) => { try { return localStorage.getItem(k); } catch { return null; } };
const store = (k: string, v: string) => { try { localStorage.setItem(k, v); } catch { /* fine */ } };

const frame = document.getElementById("frame")!;
const shield = document.getElementById("shield")!;
const bar = document.getElementById("bar")!;

let player: HTMLIFrameElement | null = null;
let info: Info | null = null;
let now = 0;
let length = 0;
let playing = true;
let seeking = false;

const cmd = (func: string, ...args: unknown[]) =>
  player?.contentWindow?.postMessage(JSON.stringify({ event: "command", func, args }), "*");
const action = (a: string, extra: Record<string, unknown> = {}) => emit("video-action", { action: a, ms: Math.round(now * 1000), ...extra });
const clock = (s: number) => {
  const t = Math.max(0, Math.floor(s));
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  return `${h ? `${h}:${String(m).padStart(2, "0")}` : m}:${String(t % 60).padStart(2, "0")}`;
};

function button(label: string, path: string, act: () => void) {
  const b = document.createElement("button");
  b.title = label;
  b.setAttribute("aria-label", label);
  b.innerHTML = `<svg viewBox="0 0 24 24"><path d="${path}"/></svg>`;
  b.onclick = act;
  return b;
}
function slider(cls: string, label: string, min: number, max: number, value: number) {
  const s = document.createElement("input");
  s.type = "range";
  s.className = cls;
  s.title = label;
  s.setAttribute("aria-label", label);
  s.min = String(min);
  s.max = String(max);
  s.value = String(value);
  return s;
}

const PLAY = "M7 5v14l11-7z";
const PAUSE = "M8 5v14M16 5v14";
const toggle = () => cmd(playing ? "pauseVideo" : "playVideo");
const play = button("Play / pause", PAUSE, toggle);
const seek = slider("b-seek", "Jump along the video", 0, 1000, 0);
seek.oninput = () => { seeking = true; };
seek.onchange = () => {
  seeking = false;
  cmd("seekTo", (Number(seek.value) / 1000) * length, true);
};
const time = document.createElement("span");
time.className = "b-time";
const volume = slider("b-vol", "Volume", 0, 100, Number(stored(VOL) ?? 100));
volume.oninput = () => { cmd("setVolume", Number(volume.value)); cmd("unMute"); store(VOL, volume.value); };
const seeIcon = document.createElement("span");
seeIcon.className = "b-label";
seeIcon.title = "See through";
seeIcon.innerHTML = '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor"/></svg>';
const see = slider("b-see", "See through the window", 25, 100, Number(stored(SEE) ?? 100));
const applySee = () => { document.documentElement.style.setProperty("--see", String(Number(see.value) / 100)); store(SEE, see.value); };
see.oninput = applySee;
applySee();
const away = button("Play it on your other computer", "M3 5h12v9H3zM7 18h4M9 14v4M15 12h6m-3-3 3 3-3 3", () => void action("send", { peer: info?.peer }));
const back = button("Back to the browser", "M4 5h16v11H4zM8 20h8M12 16v4", () => void action("back"));
const close = button("Close (stays paused in the browser)", "M6 6l12 12M18 6L6 18", () => void action("close"));
bar.append(play, seek, time, volume, seeIcon, see, away, back, close);

// Click plays or pauses; a drag moves the window.
shield.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  const { screenX: x, screenY: y } = e;
  let dragged = false;
  const move = (m: PointerEvent) => {
    if (dragged || Math.hypot(m.screenX - x, m.screenY - y) < 4) return;
    dragged = true;
    void win.startDragging();
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    if (!dragged) toggle();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
});

for (const d of ["North", "South", "West", "East", "NorthWest", "NorthEast", "SouthWest", "SouthEast"] as Parameters<typeof win.startResizeDragging>[0][]) {
  const g = document.createElement("div");
  g.className = `grip g-${d}`;
  g.onpointerdown = (e) => { if (e.button === 0) void win.startResizeDragging(d); };
  document.body.append(g);
}

window.addEventListener("message", (e) => {
  if (!player || e.source !== player.contentWindow || typeof e.data !== "string") return;
  try {
    const d = JSON.parse(e.data) as { event?: string; info?: { currentTime?: number; duration?: number; playerState?: number } };
    if (d.event !== "infoDelivery" || !d.info) return;
    if (typeof d.info.currentTime === "number") now = d.info.currentTime;
    if (typeof d.info.duration === "number") length = d.info.duration;
    if (typeof d.info.playerState === "number") {
      playing = d.info.playerState === 1 || d.info.playerState === 3;
      play.querySelector("path")?.setAttribute("d", playing ? PAUSE : PLAY);
    }
    if (!seeking && length > 0) seek.value = String((now / length) * 1000);
    time.textContent = `${clock(now)} / ${clock(length)}`;
  } catch { /* not ours */ }
});
// The bar keeps the place, for when the browser takes over again.
window.setInterval(() => { if (player) void action("time"); }, 1000);

/** Back where it was, if that screen is still there; else the bottom-right corner. */
async function place() {
  const screens = await availableMonitors();
  let r: { x: number; y: number; w: number; h: number } | null = null;
  try { r = JSON.parse(stored(KEY) ?? "null"); } catch { /* start over */ }
  const fits = r && screens.some((m) => r!.x + 40 > m.position.x && r!.y + 40 > m.position.y && r!.x + 40 < m.position.x + m.size.width && r!.y + 40 < m.position.y + m.size.height);
  if (r && fits) {
    await win.setSize(new PhysicalSize(r.w, r.h));
    await win.setPosition(new PhysicalPosition(r.x, r.y));
  } else if (screens[0]) {
    const m = screens[0];
    const s = await win.outerSize();
    const gap = Math.round(24 * m.scaleFactor);
    await win.setPosition(new PhysicalPosition(m.position.x + m.size.width - s.width - gap, m.position.y + m.size.height - s.height - gap * 3));
  }
  let timer = 0;
  const save = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(async () => {
      const [p, s] = await Promise.all([win.outerPosition(), win.outerSize()]);
      store(KEY, JSON.stringify({ x: p.x, y: p.y, w: s.width, h: s.height }));
    }, 300);
  };
  void win.onMoved(save);
  void win.onResized(save);
}

async function start() {
  info = await invoke<Info>("video_window_info");
  document.title = info.title;
  now = info.ms / 1000;
  away.hidden = !info.peer;
  if (info.peerName) away.title = `Play it on ${info.peerName}`;
  await place();
  const q = new URLSearchParams({
    autoplay: "1", playsinline: "1", enablejsapi: "1", controls: "0", disablekb: "1", fs: "0", rel: "0", iv_load_policy: "3",
    start: String(Math.floor(info.ms / 1000)), origin: location.origin,
  });
  const f = document.createElement("iframe");
  f.src = `https://www.youtube-nocookie.com/embed/${encodeURIComponent(info.id)}?${q}`;
  f.title = info.title;
  f.allow = "autoplay; encrypted-media";
  f.referrerPolicy = "strict-origin-when-cross-origin";
  f.addEventListener("load", () => {
    f.contentWindow?.postMessage(JSON.stringify({ event: "listening", id: 1 }), "*");
    cmd("setVolume", Number(volume.value));
  });
  player = f;
  frame.append(f);
  await win.show();
}
void start();
