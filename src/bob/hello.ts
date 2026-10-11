// Bob's hello: he pops up from the bottom of the opened island, waves, says who he is
// and sinks away again. Drawn on its own 640×150 canvas.

import { Sound } from "../core/sound";
import { Bob } from "./bob";

const T = {
  rise: 0.55, // pops up
  wave0: 0.7, // starts waving
  text0: 0.95, // name tag fades in
  wave1: 2.0, // stops waving
  end: 3.4, // done; the island may collapse
  autoLeave: 3.6,
  sink: 0.32,
};

const REST_Y = 78;
const R = 34;
const out = (t: number) => 1 - Math.pow(1 - t, 3);
const back = (t: number) => {
  const c1 = 1.70158;
  const c3 = c1 + 1;
  return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
};
const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

export class Greeting {
  private bob = new Bob();
  private startMs = 0;
  private tc = Number.POSITIVE_INFINITY;
  private fired = false;
  private last = 0;
  private timers: number[] = [];

  onComplete: (() => void) | null = null;

  start() {
    this.startMs = performance.now();
    this.last = this.startMs;
    this.tc = Number.POSITIVE_INFINITY;
    this.fired = false;
    this.cancelTimers();
    this.bob = new Bob();
    this.bob.setState("idle", true);
    this.timers.push(
      window.setTimeout(() => this.bob.greet(), T.rise * 600),
      window.setTimeout(() => Sound.play("peek"), 60),
      window.setTimeout(() => Sound.play("blip"), T.text0 * 1000),
      window.setTimeout(() => this.fire(), (T.end + 0.05) * 1000),
    );
  }

  /** Mouse entered the island during the greeting — hold it open. */
  hover() {
    if (this.tc >= T.autoLeave) this.tc = Number.POSITIVE_INFINITY;
  }

  /** Mouse left — sink from now. */
  interrupt() {
    const t = this.elapsed;
    if (!Number.isFinite(this.tc) || this.tc > t) this.tc = t;
    this.cancelTimers();
  }

  get elapsed(): number {
    return (performance.now() - this.startMs) / 1000;
  }

  get done(): boolean {
    return this.fired;
  }

  private fire() {
    if (this.fired) return;
    this.fired = true;
    this.cancelTimers();
    this.onComplete?.();
  }

  private cancelTimers() {
    this.timers.forEach((id) => window.clearTimeout(id));
    this.timers = [];
  }

  draw(x: CanvasRenderingContext2D) {
    const nowMs = performance.now();
    const dt = Math.min(0.05, (nowMs - this.last) / 1000);
    this.last = nowMs;
    const t = this.elapsed;
    if (!this.fired && t >= T.end && this.tc >= T.autoLeave) this.fire();

    // Vertical position: up from below, down again when leaving.
    const up = back(clamp(t / T.rise, 0, 1));
    const leave = Number.isFinite(this.tc) ? out(clamp((t - this.tc) / T.sink, 0, 1)) : 0;
    const y = REST_Y + (1 - up) * 120 + leave * 120;

    x.clearRect(0, 0, 640, 150);

    this.bob.lookX = Math.sin(t * 1.3) * 0.25;
    this.bob.lookY = 0;
    this.bob.update(dt);

    // Name tag to Bob's right, fading in.
    const ta = clamp((t - T.text0) / 0.35, 0, 1) * (1 - leave);
    if (ta > 0.01) {
      x.save();
      x.globalAlpha = ta;
      x.translate(320 + 62 + (1 - out(ta)) * -10, REST_Y - 4);
      x.fillStyle = "#f5f6f8";
      x.font = "650 22px system-ui, 'Segoe UI Variable Display', 'Segoe UI', sans-serif";
      x.textBaseline = "middle";
      x.textAlign = "left";
      x.fillText("Hi, I’m Bob", 0, 0);
      x.fillStyle = "#9398a1";
      x.font = "13px system-ui, 'Segoe UI', sans-serif";
      x.fillText("Drop a file on me any time.", 0, 24);
      x.restore();
    }

    this.bob.drawAt(x, 320 - 40, y, R);
  }
}
