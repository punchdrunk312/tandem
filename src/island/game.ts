// Catch: flick a ball to Bob and he runs to catch it. A tiny game for a spare minute.

import { h } from "./dom";
import type { ViewHost } from "./views";

export interface GameHooks {
  /** Where Bob is, in the game's own coordinates. */
  bob(): { x: number; y: number };
  /** Bob runs to this x (the game's coordinates), or back to his place (null). */
  moveBob(x: number | null): void;
  caught(streak: number, best: boolean): void;
  missed(): void;
}

const R = 9;
const G = 1150;

export function buildGame(k: GameHooks): ViewHost & { start(): void } {
  const cv = h("canvas", { class: "g-cv" }) as HTMLCanvasElement;
  const score = h("div", { class: "g-score" });
  const hint = h("div", { class: "g-hint", text: "Drag the ball and let go to throw it to Bob" });
  const el = h("div", { class: "view game" }, cv, score, hint);

  let best = 0;
  try { best = Number(localStorage.getItem("cc.catchBest")) || 0; } catch { /* fine */ }
  let streak = 0;
  type Mode = "held" | "drag" | "fly" | "caught" | "dropped";
  const ball = { x: 0, y: 0, vx: 0, vy: 0, mode: "held" as Mode, t: 0, spin: 0 };
  let samples: { t: number; x: number; y: number }[] = [];
  let bobX = 0;
  let raf = 0;
  let last = 0;
  let started = 0;

  const size = () => ({ w: el.clientWidth, h: el.clientHeight });
  const home = () => ({ x: size().w * 0.14, y: size().h * 0.4 });
  const floorY = () => k.bob().y;
  const putHome = () => {
    const p = home();
    Object.assign(ball, { x: p.x, y: p.y, vx: 0, vy: 0, mode: "held", t: 0 });
    k.moveBob(null);
  };
  const showScore = () => {
    score.textContent = `Streak ${streak} · Best ${best}`;
  };

  /** Where the ball will come down to Bob's height (walls fold it back in). */
  const landing = () => {
    const fy = floorY() - 24;
    const a = G / 2, b = ball.vy, c = ball.y - fy;
    const disc = b * b - 4 * a * c;
    if (disc < 0) return ball.x;
    const t = (-b + Math.sqrt(disc)) / (2 * a);
    const w = size().w - 2 * R;
    let x = ball.x - R + ball.vx * t;
    x = ((x % (2 * w)) + 2 * w) % (2 * w);
    return (x > w ? 2 * w - x : x) + R;
  };

  const local = (e: PointerEvent) => {
    const r = cv.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  cv.addEventListener("pointerdown", (e) => {
    const p = local(e);
    if (ball.mode !== "held" || Math.hypot(p.x - ball.x, p.y - ball.y) > 34) return;
    cv.setPointerCapture(e.pointerId);
    ball.mode = "drag";
    samples = [{ t: performance.now(), ...p }];
    hint.classList.add("off");
  });
  cv.addEventListener("pointermove", (e) => {
    if (ball.mode !== "drag") return;
    const p = local(e);
    ball.x = p.x;
    ball.y = p.y;
    samples.push({ t: performance.now(), ...p });
    samples = samples.filter((s) => performance.now() - s.t < 90);
  });
  cv.addEventListener("pointerup", () => {
    if (ball.mode !== "drag") return;
    const a = samples[0], b = samples[samples.length - 1];
    const dt = Math.max(0.016, (b.t - a.t) / 1000);
    let vx = (b.x - a.x) / dt, vy = (b.y - a.y) / dt;
    const sp = Math.hypot(vx, vy);
    if (sp > 1500) { vx *= 1500 / sp; vy *= 1500 / sp; }
    // A soft let-go is still a little lob, never a dead drop.
    if (sp < 120) { vx = 260; vy = -420; }
    Object.assign(ball, { vx, vy, mode: "fly", t: 0 });
  });

  const step = (dt: number) => {
    const { w, h: hh } = size();
    const bob = k.bob();
    bobX = bob.x;
    ball.t += dt;
    if (ball.mode === "held") ball.y = home().y + Math.sin(ball.t * 3) * 3;
    if (ball.mode === "fly" || ball.mode === "dropped") {
      ball.vy += G * dt;
      ball.x += ball.vx * dt;
      ball.y += ball.vy * dt;
      ball.spin += ball.vx * dt * 0.05;
      if (ball.x < R) { ball.x = R; ball.vx = Math.abs(ball.vx) * 0.75; }
      if (ball.x > w - R) { ball.x = w - R; ball.vx = -Math.abs(ball.vx) * 0.75; }
      if (ball.y < R) { ball.y = R; ball.vy = Math.abs(ball.vy) * 0.5; }
    }
    if (ball.mode === "fly") {
      // Bob runs to where it will land.
      k.moveBob(Math.max(30, Math.min(w - 30, landing())));
      if (ball.vy > 0 && ball.y >= bob.y - 26 && ball.y <= bob.y + 6 && Math.abs(ball.x - bob.x) < 34) {
        streak += 1;
        const record = streak > best;
        if (record) {
          best = streak;
          try { localStorage.setItem("cc.catchBest", String(best)); } catch { /* fine */ }
        }
        ball.mode = "caught";
        ball.t = 0;
        k.caught(streak, record);
        showScore();
      } else if (ball.y >= hh - R) {
        ball.mode = "dropped";
        ball.t = 0;
        streak = 0;
        k.missed();
        showScore();
      }
    }
    if (ball.mode === "dropped") {
      if (ball.y > hh - R) {
        ball.y = hh - R;
        ball.vy = -Math.abs(ball.vy) * 0.45;
        ball.vx *= 0.8;
      }
      if (ball.t > 1.1) putHome();
    }
    if (ball.mode === "caught") {
      // Held over his head for a moment, then it floats back to you.
      ball.x = bob.x;
      ball.y = bob.y - 34 - Math.sin(Math.min(1, ball.t * 3) * Math.PI) * 8;
      if (ball.t > 0.8) putHome();
    }
    draw(w, hh);
  };

  const draw = (w: number, hh: number) => {
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(hh * dpr)) {
      cv.width = Math.round(w * dpr);
      cv.height = Math.round(hh * dpr);
    }
    const x = cv.getContext("2d");
    if (!x) return;
    x.setTransform(dpr, 0, 0, dpr, 0, 0);
    x.clearRect(0, 0, w, hh);
    // Its shadow on the floor, smaller the higher it is.
    const lift = Math.max(0, Math.min(1, (hh - ball.y) / hh));
    x.fillStyle = `rgba(0,0,0,${0.22 * (1 - lift * 0.7)})`;
    x.beginPath();
    x.ellipse(ball.x, hh - 4, R * (1.3 - lift * 0.6), 2.5, 0, 0, Math.PI * 2);
    x.fill();
    // Where Bob thinks it will land.
    if (ball.mode === "fly") {
      x.strokeStyle = "rgba(45,212,191,0.35)";
      x.setLineDash([2, 3]);
      x.beginPath();
      x.moveTo(bobX - 16, hh - 4);
      x.lineTo(bobX + 16, hh - 4);
      x.stroke();
      x.setLineDash([]);
    }
    // A beach ball.
    x.save();
    x.translate(ball.x, ball.y);
    x.rotate(ball.spin);
    const cols = ["#ff5a5f", "#ffffff", "#38bdf8", "#ffffff", "#ffd23f", "#ffffff"];
    cols.forEach((c, i) => {
      x.fillStyle = c;
      x.beginPath();
      x.moveTo(0, 0);
      x.arc(0, 0, R, (i / cols.length) * Math.PI * 2, ((i + 1) / cols.length) * Math.PI * 2);
      x.closePath();
      x.fill();
    });
    x.restore();
    const g = x.createRadialGradient(ball.x - R * 0.4, ball.y - R * 0.4, 0, ball.x, ball.y, R);
    g.addColorStop(0, "rgba(255,255,255,0.55)");
    g.addColorStop(1, "rgba(0,0,0,0.12)");
    x.fillStyle = g;
    x.beginPath();
    x.arc(ball.x, ball.y, R, 0, Math.PI * 2);
    x.fill();
    if (ball.mode === "held") {
      x.strokeStyle = "rgba(127,127,127,0.35)";
      x.beginPath();
      x.arc(ball.x, ball.y, R + 5 + Math.sin(ball.t * 4) * 1.5, 0, Math.PI * 2);
      x.stroke();
    }
  };

  const frame = (t: number) => {
    if (!el.classList.contains("on") && t - started > 500) {
      raf = 0;
      k.moveBob(null);
      return;
    }
    const dt = Math.min(0.033, (t - last) / 1000);
    last = t;
    step(dt);
    raf = requestAnimationFrame(frame);
  };

  return {
    el,
    sync() {},
    start() {
      streak = 0;
      showScore();
      hint.classList.remove("off");
      // After the view has its size.
      requestAnimationFrame(() => {
        putHome();
        if (!raf) {
          last = started = performance.now();
          raf = requestAnimationFrame(frame);
        }
      });
    },
  };
}
