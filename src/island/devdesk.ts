// Browser preview only: a pretend desktop behind the bar, photographed the same way the
// app photographs the real screen (quarter size, raw, every frame), so
// Liquid Glass can be designed and checked without Windows.
//
// "#…&desk=bloom" (default), "&desk=dark" or "&desk=sunset".

import type { Island } from "./island";

type Desk = "bloom" | "dark" | "sunset";

function blob(x: CanvasRenderingContext2D, cx: number, cy: number, r: number, c: string) {
  const g = x.createRadialGradient(cx, cy, 0, cx, cy, r);
  g.addColorStop(0, c);
  g.addColorStop(1, "rgba(0,0,0,0)");
  x.fillStyle = g;
  x.fillRect(cx - r, cy - r, r * 2, r * 2);
}

/** A window of some app, so there is something with edges and words behind the glass. */
function appWindow(x: CanvasRenderingContext2D, left: number, top: number, w: number, h: number, dark: boolean) {
  x.save();
  x.shadowColor = "rgba(0,0,0,0.25)";
  x.shadowBlur = 30;
  x.fillStyle = dark ? "#1d2230" : "#ffffff";
  x.beginPath();
  x.roundRect(left, top, w, h, 12);
  x.fill();
  x.restore();
  // Tabs and an address bar.
  x.fillStyle = dark ? "#151924" : "#eef1f6";
  x.beginPath();
  x.roundRect(left, top, w, 44, [12, 12, 0, 0]);
  x.fill();
  const tabs = ["Inbox", "Design review", "Spotify – Daily Mix", "YouTube"];
  tabs.forEach((t, i) => {
    const tx = left + 14 + i * 190;
    x.fillStyle = i === 1 ? (dark ? "#1d2230" : "#ffffff") : "rgba(0,0,0,0)";
    x.beginPath();
    x.roundRect(tx, top + 8, 178, 30, 8);
    x.fill();
    x.fillStyle = ["#ea4335", "#a855f7", "#1db954", "#ff0000"][i];
    x.beginPath();
    x.arc(tx + 16, top + 23, 6, 0, Math.PI * 2);
    x.fill();
    x.fillStyle = dark ? "#c9cfdb" : "#2a3140";
    x.font = "500 13px Inter, system-ui, sans-serif";
    x.fillText(t, tx + 30, top + 28);
  });
  // Page: a headline, a picture, lines of text.
  x.fillStyle = dark ? "#e8ecf4" : "#111827";
  x.font = "700 30px Inter, system-ui, sans-serif";
  x.fillText("Weekend in Santorini", left + 40, top + 110);
  const pic = x.createLinearGradient(left + 40, top + 130, left + 420, top + 300);
  pic.addColorStop(0, "#38bdf8");
  pic.addColorStop(0.5, "#6366f1");
  pic.addColorStop(1, "#f472b6");
  x.fillStyle = pic;
  x.beginPath();
  x.roundRect(left + 40, top + 130, 380, 190, 14);
  x.fill();
  x.fillStyle = dark ? "#2a3142" : "#e5e8ef";
  for (let i = 0; i < 7; i++) {
    x.beginPath();
    x.roundRect(left + 450, top + 140 + i * 26, w - 500 - (i % 3) * 60, 12, 6);
    x.fill();
  }
}

function paint(c: HTMLCanvasElement, desk: Desk) {
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  const W = window.innerWidth, H = window.innerHeight;
  c.width = Math.round(W * dpr);
  c.height = Math.round(H * dpr);
  const x = c.getContext("2d");
  if (!x) return;
  x.scale(dpr, dpr);
  if (desk === "dark") {
    x.fillStyle = "#0b0d17";
    x.fillRect(0, 0, W, H);
    blob(x, W * 0.2, H * 0.1, 420, "rgba(124,58,237,0.75)");
    blob(x, W * 0.85, H * 0.25, 380, "rgba(6,182,212,0.6)");
    blob(x, W * 0.55, H * 1.1, 500, "rgba(236,72,153,0.5)");
    appWindow(x, W * 0.08, 70, W * 0.62, 420, true);
  } else if (desk === "sunset") {
    const sky = x.createLinearGradient(0, 0, 0, H);
    sky.addColorStop(0, "#1e3a8a");
    sky.addColorStop(0.45, "#f97316");
    sky.addColorStop(0.62, "#fde68a");
    sky.addColorStop(1, "#7c2d12");
    x.fillStyle = sky;
    x.fillRect(0, 0, W, H);
    blob(x, W * 0.5, H * 0.62, 160, "rgba(255,240,200,0.95)");
    x.fillStyle = "#3b1d47";
    x.beginPath();
    x.moveTo(0, H * 0.7);
    for (let i = 0; i <= 12; i++) x.lineTo((W / 12) * i, H * (0.62 + ((i * 37) % 11) / 60));
    x.lineTo(W, H);
    x.lineTo(0, H);
    x.fill();
  } else {
    const base = x.createLinearGradient(0, 0, W, H);
    base.addColorStop(0, "#fde2cf");
    base.addColorStop(1, "#dcd3f7");
    x.fillStyle = base;
    x.fillRect(0, 0, W, H);
    blob(x, W * 0.18, H * 0.05, 460, "rgba(255,138,92,0.85)");
    blob(x, W * 0.86, H * 0.18, 420, "rgba(79,129,255,0.8)");
    blob(x, W * 0.55, H * 1.0, 520, "rgba(168,85,247,0.6)");
    appWindow(x, W * 0.3, 60, W * 0.64, 420, false);
  }
}

export function startDevDesk(island: Island) {
  const desk = (location.hash.match(/desk=(\w+)/)?.[1] ?? "bloom") as Desk;
  const c = document.createElement("canvas");
  c.id = "dev-desk";
  Object.assign(c.style, { position: "fixed", inset: "0", width: "100%", height: "100%", zIndex: "-1" });
  document.body.prepend(c);
  paint(c, desk);
  window.addEventListener("resize", () => paint(c, desk));

  // Quarter size, raw, every frame: what the app sends.
  const shot = document.createElement("canvas");
  const sx = shot.getContext("2d", { willReadFrequently: true });
  // "&anim": something moving behind the bar (like a video), to see the glass keep up.
  const anim = location.hash.includes("anim");
  const x0 = c.getContext("2d");
  const still = document.createElement("canvas");
  still.width = c.width;
  still.height = c.height;
  still.getContext("2d")?.drawImage(c, 0, 0);
  const tick = () => {
    if (anim && x0) {
      const t = performance.now() / 1000;
      const dpr = c.width / window.innerWidth;
      x0.setTransform(1, 0, 0, 1, 0, 0);
      x0.drawImage(still, 0, 0);
      x0.scale(dpr, dpr);
      x0.fillStyle = "#22d3ee";
      x0.beginPath();
      x0.arc(window.innerWidth / 2 + Math.sin(t * 2.2) * 330, 70 + Math.cos(t * 1.7) * 40, 46, 0, Math.PI * 2);
      x0.fill();
      x0.fillStyle = "#f43f5e";
      x0.fillRect(window.innerWidth / 2 + Math.cos(t * 1.4) * 280 - 40, 40, 80, 80);
    }
    const r = island.glassRect();
    if (r && sx) {
      const k = c.width / window.innerWidth;
      const pw = Math.max(1, Math.ceil(r.w / 4)), ph = Math.max(1, Math.ceil(r.h / 4));
      shot.width = pw;
      shot.height = ph;
      sx.drawImage(c, r.x * k, r.y * k, r.w * k, r.h * k, 0, 0, pw, ph);
      island.setBackdropFrame({ ...r, pw, ph, rgba: sx.getImageData(0, 0, pw, ph).data });
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}
