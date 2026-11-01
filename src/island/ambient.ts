// The season in the bar: snow, petals, leaves, sunshine, confetti, Christmas lights or
// Halloween embers drifting quietly behind the content while the bar is open. Soft and
// sparse on purpose: it sets a mood, it never competes with what the bar says.

import type { Ambient } from "../core/seasons";

interface P {
  k: Ambient;
  x: number; y: number; vx: number; vy: number;
  r: number; rot: number; vr: number; life: number; age: number; hue: number;
}

const MAX: Record<Ambient, number> = { snow: 34, petals: 16, leaves: 12, sun: 14, confetti: 40, lights: 0, embers: 18, rain: 70, storm: 95, fog: 0, night: 0, fireworks: 0 };
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
  /** Lightning: the flash fading (1 → 0), and when the next one strikes. */
  private flash = 0;
  private nextFlash = 4;
  /** Called on each lightning strike (Bob jumps). */
  onFlash: (() => void) | null = null;
  /** How windy (0 … 1): everything drifts sideways. */
  wind = 0;
  /** Rings where raindrops land, and the puddles they make. */
  private splashes: { x: number; age: number }[] = [];
  private wet = 0;
  /** Snow settling along the bottom edge (heights in px, one per 8 px). */
  private pile: number[] = [];
  /** Seconds of rainbow left (after the rain stops). */
  private bow = 0;
  /** A shooting star crossing, and when the next one comes. */
  private shoot: { x: number; y: number; age: number } | null = null;
  private nextShoot = 5;
  /** Fireworks: rockets going up, then sparks. */
  private rockets: { x: number; y: number; vy: number; top: number; hue: number }[] = [];
  private sparks: { x: number; y: number; vx: number; vy: number; age: number; hue: number }[] = [];
  /** A bat flapping across (Halloween). */
  private bat: { x: number; y: number; dir: number } | null = null;

  /** The rain has stopped: a rainbow for a minute. */
  rainbow() {
    this.bow = 60;
  }

  /** A one-off shower (the year turning, a birthday). */
  celebrate() {
    this.burst = 1.6;
  }

  get busy(): boolean {
    return this.kind != null || this.burst > 0 || this.ps.length > 0 || this.flash > 0 || this.bow > 0 || this.wet > 0 || this.sparks.length > 0 || this.pile.some((v) => v > 0.2);
  }

  step(dt: number, w: number, h: number) {
    this.t += dt;
    const kind = this.burst > 0 ? "confetti" : this.kind;
    this.burst = Math.max(0, this.burst - dt);
    this.flash = Math.max(0, this.flash - dt * 2.6);
    this.bow = Math.max(0, this.bow - dt);
    const raining = kind === "rain" || kind === "storm";
    this.wet = Math.max(0, Math.min(1, this.wet + (raining ? dt / 20 : -dt / 60)));
    for (const s of this.splashes) s.age += dt;
    this.splashes = this.splashes.filter((s) => s.age < 0.45);
    // Snow piles up while it snows (a little on each flake that lands), and melts after.
    const cols = Math.ceil(w / 8) + 1;
    if (this.pile.length !== cols) this.pile = Array.from({ length: cols }, (_, i) => this.pile[i] ?? 0);
    if (kind !== "snow") this.pile = this.pile.map((v) => Math.max(0, v - dt * 0.4));
    if (kind === "night") {
      this.nextShoot -= dt;
      if (this.nextShoot <= 0 && !this.shoot) {
        this.shoot = { x: w * (0.3 + Math.random() * 0.6), y: 4 + Math.random() * h * 0.3, age: 0 };
        this.nextShoot = 8 + Math.random() * 14;
      }
    }
    if (this.shoot && (this.shoot.age += dt) > 0.9) this.shoot = null;
    if (kind === "fireworks" && Math.random() < dt * 0.9 && this.rockets.length < 3) {
      this.rockets.push({ x: w * (0.1 + Math.random() * 0.8), y: h + 4, vy: -(170 + Math.random() * 60), top: h * (0.15 + Math.random() * 0.35), hue: Math.random() * 360 });
    }
    for (const r of this.rockets) r.y += r.vy * dt;
    for (const r of this.rockets.filter((r) => r.y <= r.top)) {
      for (let i = 0; i < 28; i++) {
        const a = (i / 28) * Math.PI * 2, sp = 50 + Math.random() * 40;
        this.sparks.push({ x: r.x, y: r.y, vx: Math.cos(a) * sp, vy: Math.sin(a) * sp, age: 0, hue: r.hue + Math.random() * 30 });
      }
    }
    this.rockets = this.rockets.filter((r) => r.y > r.top);
    for (const s of this.sparks) {
      s.age += dt;
      s.x += s.vx * dt;
      s.y += s.vy * dt;
      s.vy += 60 * dt;
      s.vx *= 1 - dt * 1.5;
    }
    this.sparks = this.sparks.filter((s) => s.age < 1.4);
    if (kind === "embers" && !this.bat && Math.random() < dt / 12) {
      const dir = Math.random() < 0.5 ? 1 : -1;
      this.bat = { x: dir > 0 ? -20 : w + 20, y: h * (0.2 + Math.random() * 0.4), dir };
    }
    if (this.bat) {
      this.bat.x += this.bat.dir * 90 * dt;
      this.bat.y += Math.sin(this.t * 4) * 12 * dt;
      if (this.bat.x < -30 || this.bat.x > w + 30) this.bat = null;
    }
    if (kind === "storm") {
      this.nextFlash -= dt;
      if (this.nextFlash <= 0) {
        // Sometimes a double strike, like the real thing.
        this.flash = 1;
        this.nextFlash = Math.random() < 0.3 ? 0.25 : 6 + Math.random() * 9;
        this.onFlash?.();
      }
    }
    if (kind && kind !== "lights") {
      const want = MAX[kind] * (this.burst > 0 ? 2 : 1);
      const rate = this.burst > 0 ? 60 : kind === "rain" || kind === "storm" ? want * 4 : want / 5; // per second
      if (this.ps.length < want && Math.random() < rate * dt) this.ps.push(this.spawn(kind, w, h));
    }
    for (const p of this.ps) {
      p.age += dt;
      p.x += (p.vx + this.wind * (p.k === "rain" || p.k === "storm" ? 120 : 45)) * dt;
      p.y += p.vy * dt;
      // A drop hitting the bottom splashes; a flake settles on the pile.
      if ((p.k === "rain" || p.k === "storm") && p.y >= h - 3 && p.age < p.life) {
        if (this.splashes.length < 24) this.splashes.push({ x: p.x, age: 0 });
        p.age = p.life;
      } else if (p.k === "snow" && p.y >= h - 2 - (this.pile[Math.floor(p.x / 8)] ?? 0) && p.age < p.life) {
        const i = Math.floor(p.x / 8);
        for (const j of [i - 1, i, i + 1]) if (this.pile[j] != null) this.pile[j] = Math.min(9, this.pile[j] + (j === i ? 0.9 : 0.45));
        p.age = p.life;
      }
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
      case "rain":
      case "storm": {
        // Slanting streaks; a storm's are faster and blown harder.
        const s = kind === "storm" ? 1.3 : 1;
        return { ...base, x: rnd(0, w + 40), y: rnd(-20, -4), vx: -40 * s, vy: rnd(300, 380) * s, r: rnd(6, 11), life: 2, rot: 0, vr: 0 };
      }
      case "embers":
        return { ...base, x: rnd(0, w), y: h + 4, vx: rnd(-6, 6), vy: rnd(-18, -8), r: rnd(0.8, 1.8), life: rnd(4, 8) };
      default:
        return { ...base, x: 0, y: 0, vx: 0, vy: 0, r: 0, life: 0 };
    }
  }

  draw(x: CanvasRenderingContext2D, w: number, h: number) {
    x.clearRect(0, 0, w, h);
    x.save();
    // On the glass whatever is behind shows through: white with a soft dark edge reads
    // on a bright wallpaper and a dark one alike.
    if (this.light) {
      x.shadowColor = "rgba(10,25,50,0.35)";
      x.shadowBlur = 2;
    }
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
    if (kind === "night") this.drawNight(x, w, h);
    if (this.bow > 0) this.drawRainbow(x, w, h);
    if (kind === "fog") this.drawFog(x, w, h);
    if ((kind === "storm" || kind === "rain") && !this.light) {
      // A low, grey sky.
      const g = x.createLinearGradient(0, 0, 0, h);
      g.addColorStop(0, this.light ? "rgba(70,85,105,0.10)" : "rgba(120,140,170,0.10)");
      g.addColorStop(1, "rgba(120,140,170,0)");
      x.fillStyle = g;
      x.fillRect(0, 0, w, h);
    }
    // Puddles along the bottom while it's wet, and the rings of drops landing in them.
    if (this.wet > 0) {
      const c = this.light ? "255,255,255" : "170,200,240";
      for (const [fx, fw] of [[0.18, 60], [0.52, 90], [0.83, 50]] as const) {
        x.fillStyle = `rgba(${c},${0.12 * this.wet})`;
        x.beginPath();
        x.ellipse(w * fx, h - 2, fw * this.wet, 3, 0, 0, Math.PI * 2);
        x.fill();
      }
      x.strokeStyle = `rgba(${c},0.5)`;
      x.lineWidth = 1;
      for (const s of this.splashes) {
        const k = s.age / 0.45;
        x.globalAlpha = 1 - k;
        x.beginPath();
        x.ellipse(s.x, h - 3, 2 + k * 8, 1 + k * 2, 0, 0, Math.PI * 2);
        x.stroke();
      }
      x.globalAlpha = 1;
    }
    if (this.pile.some((v) => v > 0.2)) {
      x.fillStyle = "rgba(245,250,255,0.92)";
      x.beginPath();
      x.moveTo(0, h);
      this.pile.forEach((v, i) => x.lineTo(i * 8, h - v));
      x.lineTo(w, h);
      x.closePath();
      x.fill();
    }
    for (const s of this.sparks) {
      x.fillStyle = `hsla(${s.hue},95%,65%,${Math.max(0, 1 - s.age / 1.4)})`;
      x.fillRect(s.x - 1, s.y - 1, 2, 2);
    }
    for (const r of this.rockets) {
      x.fillStyle = `hsla(${r.hue},90%,75%,0.9)`;
      x.fillRect(r.x - 1, r.y, 2, 6);
    }
    if (this.bat) this.drawBat(x, this.bat.x, this.bat.y, this.bat.dir);
    if (this.flash > 0) {
      x.shadowBlur = 0;
      x.fillStyle = `rgba(225,235,255,${(this.light ? 0.5 : 0.32) * this.flash * this.flash})`;
      x.fillRect(0, 0, w, h);
    }
    for (const p of this.ps) {
      const fade = p.k === "rain" || p.k === "storm" ? 1 : Math.min(1, p.age / 0.6, (p.life - p.age) / 0.8);
      x.save();
      x.globalAlpha = Math.max(0, fade);
      x.translate(p.x, p.y);
      x.rotate(p.rot);
      switch (p.k) {
        case "snow":
          x.fillStyle = this.light ? "rgba(255,255,255,0.95)" : "rgba(255,255,255,0.55)";
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
        case "rain":
        case "storm":
          x.rotate(-p.rot);
          x.strokeStyle = this.light ? "rgba(255,255,255,0.85)" : "rgba(190,215,255,0.42)";
          x.lineWidth = 1;
          x.beginPath();
          x.moveTo(0, 0);
          x.lineTo(p.vx * 0.022, p.vy * 0.022 * (p.r / 9));
          x.stroke();
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
    x.restore();
  }

  /** Stars twinkling, the moon in today's phase, and a shooting star now and then. */
  private drawNight(x: CanvasRenderingContext2D, w: number, h: number) {
    for (let i = 0; i < 26; i++) {
      // Fixed places (a cheap hash), each twinkling at its own pace.
      const sx = ((i * 2654435761) % 1000) / 1000 * w;
      const sy = ((i * 40503) % 1000) / 1000 * h * 0.85;
      const a = 0.25 + 0.35 * (0.5 + 0.5 * Math.sin(this.t * (1 + (i % 5) * 0.4) + i));
      x.fillStyle = `rgba(255,255,240,${this.light ? Math.min(1, a + 0.3) : a})`;
      x.fillRect(sx, sy, i % 4 === 0 ? 1.6 : 1, i % 4 === 0 ? 1.6 : 1);
    }
    // The moon: lit by today's phase (a 29.53-day cycle from a known new moon).
    const phase = (((Date.now() - Date.UTC(2000, 0, 6, 18, 14)) / 864e5) % 29.53) / 29.53;
    const mx = w - 46, my = 22, r = 9;
    x.save();
    x.fillStyle = this.light ? "rgba(250,240,200,0.9)" : "rgba(255,246,214,0.92)";
    x.shadowColor = "rgba(255,240,200,0.6)";
    x.shadowBlur = 10;
    x.beginPath();
    x.arc(mx, my, r, 0, Math.PI * 2);
    x.fill();
    x.shadowBlur = 0;
    // The dark part: a disc slid across.
    x.globalCompositeOperation = "destination-out";
    const off = (phase < 0.5 ? 1 - phase * 4 : -(1 - (phase - 0.5) * 4)) * r * 2;
    x.beginPath();
    x.arc(mx + off, my, r, 0, Math.PI * 2);
    x.fill();
    x.restore();
    if (this.shoot) {
      const s = this.shoot, k = s.age / 0.9;
      const hx = s.x - k * 140, hy = s.y + k * 50;
      const g = x.createLinearGradient(hx, hy, hx + 40, hy - 14);
      g.addColorStop(0, `rgba(255,255,255,${0.9 * (1 - k)})`);
      g.addColorStop(1, "rgba(255,255,255,0)");
      x.strokeStyle = g;
      x.lineWidth = 1.5;
      x.beginPath();
      x.moveTo(hx, hy);
      x.lineTo(hx + 40, hy - 14);
      x.stroke();
    }
  }

  /** A soft rainbow arcing behind, fading in and out. */
  private drawRainbow(x: CanvasRenderingContext2D, w: number, h: number) {
    const a = Math.min(1, this.bow / 4, (60 - this.bow) / 3) * 0.22;
    const cols = ["#ff5a5f", "#ff9f40", "#ffd23f", "#4ade80", "#38bdf8", "#818cf8", "#c084fc"];
    x.save();
    x.globalAlpha = a;
    x.lineWidth = 4;
    cols.forEach((c, i) => {
      x.strokeStyle = c;
      x.beginPath();
      x.arc(w * 0.68, h + 30, 150 - i * 4, Math.PI * 1.05, Math.PI * 1.95);
      x.stroke();
    });
    x.restore();
  }

  private drawBat(x: CanvasRenderingContext2D, bx: number, by: number, dir: number) {
    const f = Math.sin(this.t * 18) * 5;
    x.save();
    x.translate(bx, by);
    x.scale(dir, 1);
    x.fillStyle = this.light ? "rgba(40,30,50,0.75)" : "rgba(20,12,28,0.9)";
    x.beginPath();
    x.moveTo(0, 0);
    x.quadraticCurveTo(-6, -4 - f, -12, -2 - f);
    x.quadraticCurveTo(-8, 0, -9, 3);
    x.quadraticCurveTo(-4, 1, 0, 3);
    x.quadraticCurveTo(4, 1, 9, 3);
    x.quadraticCurveTo(8, 0, 12, -2 - f);
    x.quadraticCurveTo(6, -4 - f, 0, 0);
    x.fill();
    x.restore();
  }

  /** Fog: soft bands drifting slowly across. */
  private drawFog(x: CanvasRenderingContext2D, w: number, h: number) {
    for (let i = 0; i < 4; i++) {
      const cx = ((this.t * (8 + i * 4) + i * 230) % (w + 400)) - 200;
      const cy = h * (0.25 + i * 0.2);
      const g = x.createRadialGradient(cx, cy, 0, cx, cy, 190);
      const c = this.light ? "255,255,255" : "210,220,235";
      g.addColorStop(0, `rgba(${c},${this.light ? 0.3 : 0.13})`);
      g.addColorStop(1, `rgba(${c},0)`);
      x.fillStyle = g;
      x.fillRect(0, 0, w, h);
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
