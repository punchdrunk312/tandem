// The season in the bar: snow, petals, leaves, sunshine, confetti, Christmas lights or
// Halloween embers drifting quietly behind the content while the bar is open. Soft and
// sparse on purpose: it sets a mood, it never competes with what the bar says.

import type { Ambient } from "../core/seasons";

interface P {
  k: Ambient;
  x: number; y: number; vx: number; vy: number;
  r: number; rot: number; vr: number; life: number; age: number; hue: number;
}

const MAX: Record<Ambient, number> = { snow: 34, petals: 16, leaves: 12, sun: 14, confetti: 40, lights: 0, embers: 18 };
const LEAF = ["#d9822b", "#c2552a", "#e3a33a", "#9c4a1f"];
const CONFETTI = ["#ff6b9a", "#ffd23f", "#2dd4bf", "#7c5cff", "#38bdf8", "#fb923c"];
const BULBS = ["#ff5a5f", "#ffd23f", "#2dd4bf", "#38bdf8", "#a78bfa"];

export class AmbientLayer {
  kind: Ambient | null = null;
  /** On the light glass bar: snow and the like in colours that show on white. */
  light = false;
  private ps: P[] = [];
  private t = 0;
  private burst = 0;

  /** A one-off shower (the year turning, a birthday). */
  celebrate() {
    this.burst = 1.6;
  }

  get busy(): boolean {
    return this.kind != null || this.burst > 0 || this.ps.length > 0;
  }

  step(dt: number, w: number, h: number) {
    this.t += dt;
    const kind = this.burst > 0 ? "confetti" : this.kind;
    this.burst = Math.max(0, this.burst - dt);
    if (kind && kind !== "lights") {
      const want = MAX[kind] * (this.burst > 0 ? 2 : 1);
      const rate = this.burst > 0 ? 60 : want / 5; // per second
      if (this.ps.length < want && Math.random() < rate * dt) this.ps.push(this.spawn(kind, w, h));
    }
    for (const p of this.ps) {
      p.age += dt;
      p.x += p.vx * dt;
      p.y += p.vy * dt;
      p.rot += p.vr * dt;
      if (kind === "leaves" || kind === "petals") p.vx += Math.sin(this.t * 1.3 + p.hue) * 6 * dt; // a little sway
    }
    this.ps = this.ps.filter((p) => p.age < p.life && p.y < h + 12 && p.y > -16 && p.x > -16 && p.x < w + 16);
  }

  private spawn(kind: Ambient, w: number, h: number): P {
    const rnd = (a: number, b: number) => a + Math.random() * (b - a);
    const base = { k: kind, rot: rnd(0, 6.28), vr: rnd(-1.5, 1.5), age: 0, hue: Math.random() * 6 };
    switch (kind) {
      case "snow":
        return { ...base, x: rnd(0, w), y: -4, vx: rnd(-6, 6), vy: rnd(10, 22), r: rnd(0.8, 2.2), life: 30 };
      case "petals":
        return { ...base, x: rnd(-10, w * 0.7), y: -6, vx: rnd(8, 20), vy: rnd(8, 16), r: rnd(2.2, 3.6), life: 30 };
      case "leaves":
        return { ...base, x: rnd(0, w), y: -8, vx: rnd(-8, 8), vy: rnd(12, 22), r: rnd(3, 5), life: 30, vr: rnd(-2.5, 2.5) };
      case "sun":
        return { ...base, x: rnd(w * 0.45, w), y: rnd(0, h), vx: rnd(-4, 4), vy: rnd(-5, -1), r: rnd(0.8, 1.8), life: rnd(3, 6) };
      case "confetti":
        return { ...base, x: rnd(0, w), y: -6, vx: rnd(-20, 20), vy: rnd(40, 90), r: rnd(2, 3.4), life: 8, vr: rnd(-6, 6) };
      case "embers":
        return { ...base, x: rnd(0, w), y: h + 4, vx: rnd(-6, 6), vy: rnd(-18, -8), r: rnd(0.8, 1.8), life: rnd(4, 8) };
      default:
        return { ...base, x: 0, y: 0, vx: 0, vy: 0, r: 0, life: 0 };
    }
  }

  draw(x: CanvasRenderingContext2D, w: number, h: number) {
    x.clearRect(0, 0, w, h);
    const kind = this.burst > 0 ? "confetti" : this.kind;
    if (kind === "sun") {
      // Warm light in the top-right corner, breathing slowly.
      const a = 0.1 + 0.04 * Math.sin(this.t * 0.8);
      const g = x.createRadialGradient(w - 30, -10, 4, w - 30, -10, 220);
      g.addColorStop(0, `rgba(255,214,120,${a})`);
      g.addColorStop(1, "rgba(255,214,120,0)");
      x.fillStyle = g;
      x.fillRect(0, 0, w, h);
    }
    if (kind === "lights") this.drawLights(x, w, h);
    for (const p of this.ps) {
      const fade = Math.min(1, p.age / 0.6, (p.life - p.age) / 0.8);
      x.save();
      x.globalAlpha = Math.max(0, fade);
      x.translate(p.x, p.y);
      x.rotate(p.rot);
      switch (p.k) {
        case "snow":
          x.fillStyle = this.light ? "rgba(110,150,200,0.5)" : "rgba(255,255,255,0.55)";
          x.beginPath();
          x.arc(0, 0, p.r, 0, Math.PI * 2);
          x.fill();
          break;
        case "petals":
          x.fillStyle = "rgba(255,170,205,0.55)";
          x.beginPath();
          x.ellipse(0, 0, p.r, p.r * 0.55, 0, 0, Math.PI * 2);
          x.fill();
          break;
        case "leaves":
          x.fillStyle = LEAF[Math.floor(p.hue) % LEAF.length];
          x.globalAlpha *= 0.6;
          x.beginPath();
          x.moveTo(0, -p.r);
          x.quadraticCurveTo(p.r * 0.9, 0, 0, p.r);
          x.quadraticCurveTo(-p.r * 0.9, 0, 0, -p.r);
          x.fill();
          break;
        case "sun":
          x.fillStyle = this.light ? "rgba(235,170,60,0.55)" : "rgba(255,226,150,0.5)";
          x.beginPath();
          x.arc(0, 0, p.r, 0, Math.PI * 2);
          x.fill();
          break;
        case "confetti":
          x.fillStyle = CONFETTI[Math.floor(p.hue) % CONFETTI.length];
          x.globalAlpha *= 0.85;
          x.fillRect(-p.r, -p.r * 0.4, p.r * 2, p.r * 0.8);
          break;
        case "embers": {
          const g = x.createRadialGradient(0, 0, 0, 0, 0, p.r * 3);
          g.addColorStop(0, "rgba(255,170,60,0.8)");
          g.addColorStop(1, "rgba(255,120,30,0)");
          x.fillStyle = g;
          x.beginPath();
          x.arc(0, 0, p.r * 3, 0, Math.PI * 2);
          x.fill();
          break;
        }
      }
      x.restore();
    }
  }

  /** A string of Christmas lights along the bottom edge, twinkling. */
  private drawLights(x: CanvasRenderingContext2D, w: number, h: number) {
    const y0 = h - 7;
    x.save();
    x.strokeStyle = this.light ? "rgba(60,90,70,0.45)" : "rgba(40,60,50,0.6)";
    x.lineWidth = 1;
    x.beginPath();
    for (let px = 0; px <= w; px += 4) {
      const py = y0 + Math.sin(px / 46) * 2.5;
      if (px === 0) x.moveTo(px, py);
      else x.lineTo(px, py);
    }
    x.stroke();
    let i = 0;
    for (let px = 14; px < w; px += 26, i++) {
      const py = y0 + Math.sin(px / 46) * 2.5 + 3;
      const c = BULBS[i % BULBS.length];
      const tw = 0.55 + 0.45 * Math.sin(this.t * 2.2 + i * 1.7);
      const g = x.createRadialGradient(px, py, 0, px, py, 9);
      g.addColorStop(0, hexA(c, 0.55 * tw));
      g.addColorStop(1, hexA(c, 0));
      x.fillStyle = g;
      x.fillRect(px - 9, py - 9, 18, 18);
      x.fillStyle = hexA(c, 0.6 + 0.4 * tw);
      x.beginPath();
      x.ellipse(px, py, 2.2, 3, 0, 0, Math.PI * 2);
      x.fill();
    }
    x.restore();
  }
}

function hexA(hex: string, a: number): string {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
}
