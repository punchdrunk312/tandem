// Bob — Tandem's own mascot, drawn entirely in code (no image files).
//
// A rounded teal cube with an amber twin tucked behind him (the two squares of the
// Tandem logo), a little antenna whose bulb shows what is going on, big glossy eyes
// that follow the cursor, and a mouth that changes with his mood.
//
// The animation plumbing (tweens, look-at, blink, particles) is the generic part of
// Coucou's MIT-licensed engine. Everything you can see — shape, colours, face,
// hands, badges — is original to Tandem.

import { Ease, lerp, type EaseFn } from "../core/anim";
import { Sound } from "../core/sound";
import type { BotEmoteName, BotStateName } from "../core/layout";

export type EyeShape =
  | "pill" | "wide" | "dot" | "line" | "flat" | "happy" | "closed"
  | "spiral" | "heart" | "star" | "tired" | "wink" | "cup";

export type BadgeKind = "dots" | "bang" | "question" | "dot";

export interface Badge {
  kind: BadgeKind;
  color: RGB;
}

export type RGB = readonly [number, number, number]; // components 0…1

export type TweenKey = readonly [target: number, durationMs: number, ease: EaseFn];

interface Tween {
  prop: PropKey;
  keys: TweenKey[];
  index: number;
  from: number;
  startMs: number;
  onComplete?: () => void;
}

type PropKey =
  | "yaw" | "pitch" | "roll" | "tilt" | "open" | "sx" | "sy"
  | "oy" | "ox" | "tint" | "morph" | "hands" | "blush" | "es" | "badgeS";

interface BotStateCfg {
  color: RGB;
  tint: number;
  eye: EyeShape;
  mouth: MouthShape;
  badge: Badge | null;
  bounces: boolean;
  scans: boolean;
  breathes: boolean;
  zz: boolean;
  sweat: boolean;
  look: readonly [number, number] | null;
  tilt: number;
}

type MouthShape = "smile" | "grin" | "flat" | "o" | "wavy" | "tiny" | "hmm";

interface Particle {
  type: "heart" | "star" | "spark" | "sweat" | "z";
  x: number; y: number; vx: number; vy: number;
  age: number; life: number; rot: number; size: number;
}

// ── Look ──────────────────────────────────────────────────────────────────────

const TEAL_TOP: RGB = [0.45, 0.93, 0.85]; // light, top-left
const TEAL_BOTTOM: RGB = [0.08, 0.72, 0.64]; // deep, bottom-right
const AMBER: RGB = [0.98, 0.75, 0.14];
const AMBER_DEEP: RGB = [0.93, 0.55, 0.08];
const INK = "rgb(10,28,34)";

const C = {
  idle: [0.18, 0.83, 0.75] as RGB,
  working: [0.231, 0.62, 1] as RGB,
  thinking: [0.545, 0.361, 0.965] as RGB,
  searching: [0.388, 0.396, 0.949] as RGB,
  approval: [0.961, 0.647, 0.141] as RGB,
  question: [0.133, 0.827, 0.933] as RGB,
  error: [0.957, 0.314, 0.369] as RGB,
  finished: [0.204, 0.831, 0.6] as RGB,
  ratelimit: [0.984, 0.573, 0.235] as RGB,
  sleeping: [0.58, 0.635, 0.722] as RGB,
  dizzy: [0.957, 0.447, 0.714] as RGB,
};

const base = {
  bounces: false, scans: false, breathes: false, zz: false, sweat: false,
  look: null, tilt: 0,
};

export const BOT_STATES: Record<BotStateName, BotStateCfg> = {
  idle: { ...base, color: C.idle, tint: 0, eye: "pill", mouth: "smile", badge: null },
  working: { ...base, color: C.working, tint: 0.5, eye: "pill", mouth: "smile", badge: { kind: "dots", color: C.working } },
  thinking: { ...base, color: C.thinking, tint: 0.5, eye: "pill", mouth: "hmm", badge: { kind: "dots", color: C.thinking }, look: [0.55, 0.55] },
  searching: { ...base, color: C.searching, tint: 0.5, eye: "pill", mouth: "tiny", badge: { kind: "dots", color: C.searching }, scans: true },
  approval: { ...base, color: C.approval, tint: 0.55, eye: "wide", mouth: "o", badge: { kind: "bang", color: C.approval }, bounces: true },
  question: { ...base, color: C.question, tint: 0.5, eye: "pill", mouth: "hmm", badge: { kind: "question", color: C.question }, tilt: 0.17 },
  error: { ...base, color: C.error, tint: 0.6, eye: "flat", mouth: "flat", badge: { kind: "dot", color: C.error } },
  finished: { ...base, color: C.finished, tint: 0.2, eye: "happy", mouth: "grin", badge: { kind: "dot", color: C.finished } },
  ratelimit: { ...base, color: C.ratelimit, tint: 0.5, eye: "tired", mouth: "wavy", badge: { kind: "dot", color: C.ratelimit }, sweat: true },
  sleeping: { ...base, color: C.sleeping, tint: 0.45, eye: "closed", mouth: "tiny", badge: null, breathes: true, zz: true },
  dizzy: { ...base, color: C.dizzy, tint: 0.5, eye: "spiral", mouth: "wavy", badge: null },
};

/** State → sound. */
export const STATE_SOUND: Partial<Record<BotStateName, string>> = {
  working: "work", thinking: "think", searching: "search", approval: "approval",
  question: "question", error: "error", finished: "finish", ratelimit: "rate",
  sleeping: "sleep", dizzy: "dizzy",
};

const EMOTE_EYE: Record<BotEmoteName, EyeShape> = {
  love: "heart", surprised: "dot", proud: "star", wink: "wink",
  yawn: "tired", happy: "happy", annoyed: "line",
};

// ── Small helpers ─────────────────────────────────────────────────────────────

const now = () => performance.now() / 1000;

export function hexToRGB(hex: string): RGB {
  const h = hex.replace("#", "");
  const v = parseInt(h, 16);
  return [((v >> 16) & 255) / 255, ((v >> 8) & 255) / 255, (v & 255) / 255];
}

const rgba = (c: RGB, a = 1) =>
  `rgba(${Math.round(c[0] * 255)},${Math.round(c[1] * 255)},${Math.round(c[2] * 255)},${a})`;

const mix3 = (a: RGB, b: RGB, t: number): RGB => [
  lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t),
];

function roundRectPath(x: CanvasRenderingContext2D, X: number, Y: number, W: number, H: number, R: number) {
  const r = Math.max(0, Math.min(R, W / 2, H / 2));
  x.beginPath();
  x.moveTo(X + r, Y);
  x.arcTo(X + W, Y, X + W, Y + H, r);
  x.arcTo(X + W, Y + H, X, Y + H, r);
  x.arcTo(X, Y + H, X, Y, r);
  x.arcTo(X, Y, X + W, Y, r);
  x.closePath();
}

function heartPath(x: CanvasRenderingContext2D, s: number) {
  x.beginPath();
  x.moveTo(0, s * 0.38);
  x.bezierCurveTo(-s * 1.05, -s * 0.15, -s * 0.5, -s * 0.95, 0, -s * 0.38);
  x.bezierCurveTo(s * 0.5, -s * 0.95, s * 1.05, -s * 0.15, 0, s * 0.38);
  x.closePath();
}

function starPath(x: CanvasRenderingContext2D, ro: number, ri: number) {
  x.beginPath();
  for (let i = 0; i < 10; i++) {
    const r = i % 2 ? ri : ro;
    const a = -Math.PI / 2 + (i * Math.PI) / 5;
    x.lineTo(Math.cos(a) * r, Math.sin(a) * r);
  }
  x.closePath();
}

const FONT = `system-ui, "Segoe UI Variable Text", "Segoe UI", sans-serif`;

// ── Engine ────────────────────────────────────────────────────────────────────

export class Bob {
  isMini = false;
  /** Optional solid body colour (unused by Tandem; null = Bob's own teal). */
  bodyColor: RGB | null = null;

  // Animated state (BotEngine `s`)
  yaw = 0; pitch = 0; roll = 0; tilt = 0; open = 1;
  sx = 1; sy = 1; oy = 0; ox = 0;
  tint = 0; morph = 0; hands = 0; blush = 0; es = 1; badgeS = 0;

  // Targets
  tgYaw = 0; tgPitch = 0; tgTilt = 0; tgSy = 1; tgSx = 1; tgEs = 1;

  /** Extra canvas height above the body so hearts can fly out without clipping. */
  particleOverhang = 0;

  // Mouth spring (fraction of R)
  slotH = 0; slotHTarget = 0; slotHVel = 0; isChewing = false;

  col: RGB = C.idle;
  colT: RGB = C.idle;

  state: BotStateName = "idle";
  cfg: BotStateCfg = BOT_STATES.idle;

  eyeOverride: EyeShape | null = null;
  eyeOverrideUntil = 0;
  permanentEye: EyeShape | null = null;
  permanentEmote: BotEmoteName | null = null;
  miniNextBehavior = 0;

  badge: Badge | null = null;
  private badgeKey = "none";
  private badgeToken = 0;

  private tweens = new Map<PropKey, Tween>();
  private locks = new Set<PropKey>();
  private particles: Particle[] = [];

  lookX = 0;
  lookY = 0;

  lastTime = now();
  private t0 = now() - Math.random() * 5;
  private nextBlink = now() + 1.5 + Math.random() * 2;
  waveUntil = 0;
  waveStart = 0;
  private greetToken = 0;
  private lastAmbient = 0;
  private slapTimes: number[] = [];
  private miniLookTarget = { x: 0, y: 0 };
  private miniLookNextTime = 0;

  /** Fired when three slaps land inside 1.7 s (→ dizzy + confused view). */
  onDizzy: (() => void) | null = null;

  // ── Public API ──────────────────────────────────────────────────────────────

  setState(next: BotStateName, force = false) {
    if (this.state === next && !force) return;
    const prev = this.state;
    this.state = next;
    this.cfg = BOT_STATES[next];
    this.colT = this.cfg.color;
    if (!this.locks.has("tint")) this.tint = this.cfg.tint;
    if (!this.locks.has("tilt")) this.tgTilt = this.cfg.tilt;
    this.setBadge(this.cfg.badge);

    switch (next) {
      case "finished":
        this.doRoll(950, 1);
        setTimeout(() => this.emit("spark", 5), 500);
        break;
      case "error":
        this.anim("ox", [
          [0.08, 50, Ease.out], [-0.08, 70, Ease.inOut],
          [0.05, 70, Ease.inOut], [0, 90, Ease.out],
        ]);
        break;
      case "approval":
        this.anim("oy", [[-0.2, 150, Ease.out], [0, 300, Ease.back]]);
        break;
      case "dizzy":
        this.doRoll(1300, 2);
        break;
      case "question":
        this.blink();
        break;
      case "ratelimit":
        this.emit("sweat", 1);
        break;
      default:
        if (prev !== "idle" || next !== "idle") this.blink();
    }
  }

  setBadge(b: Badge | null) {
    const key = b ? `${b.kind}-${b.color.join(",")}` : "none";
    if (key === this.badgeKey) return;
    this.badgeKey = key;
    const tok = ++this.badgeToken;
    this.anim("badgeS", [[0, 90, Ease.inOut]]);
    setTimeout(() => {
      if (tok !== this.badgeToken) return;
      this.badge = b;
      if (b) this.anim("badgeS", [[1, 280, Ease.back]]);
    }, 100);
  }

  blink() {
    if (this.locks.has("open")) return;
    this.anim("open", [[0.06, 70, Ease.inOut], [1, 130, Ease.out]]);
  }

  squash() {
    this.anim("sy", [[0.78, 70, Ease.out], [1.1, 130, Ease.out], [1, 170, Ease.inOut]]);
    this.anim("sx", [[1.16, 70, Ease.out], [0.95, 130, Ease.out], [1, 170, Ease.inOut]]);
  }

  /** Mailbox swallow — opens the slot, chews, then closes. */
  gulp() {
    this.slotHTarget = 0.42;
    setTimeout(() => {
      this.slotHTarget = 0;
      this.isChewing = true;
      setTimeout(() => { this.isChewing = false; }, 800);
    }, 460);
    this.anim("sy", [[0.78, 80, Ease.out], [1.18, 130, Ease.out], [1, 220, Ease.back]]);
    this.anim("sx", [[1.28, 80, Ease.out], [0.92, 130, Ease.out], [1, 220, Ease.back]]);
    this.blink();
  }

  slap() {
    this.interruptGreet();
    if (this.state === "dizzy") return;
    const t = now();
    this.slapTimes = this.slapTimes.filter((s) => t - s < 1.7);
    this.slapTimes.push(t);
    Sound.play("slap");
    this.squash();
    if (this.slapTimes.length >= 3) {
      this.slapTimes = [];
      this.onDizzy?.();
    } else {
      this.eyeOverride = "line";
      this.eyeOverrideUntil = t + 0.8;
      setTimeout(() => Sound.play("annoyed"), 60);
    }
  }

  doRoll(durationMs: number, turns: number) {
    this.roll = 0;
    this.anim("roll", [[Math.PI * 2 * turns, durationMs, Ease.inOut]], () => { this.roll = 0; });
  }

  /** Peek wave (hello). */
  greet() {
    const t = now();
    const tok = ++this.greetToken;
    this.waveStart = t + 0.45;
    this.waveUntil = t + 1.55;

    this.eyeOverride = "happy";
    this.eyeOverrideUntil = t + 2.0;
    this.anim("oy", [[-0.06, 220, Ease.out], [0.0, 220, Ease.back]]);

    setTimeout(() => {
      if (this.greetToken !== tok) return;
      this.anim("hands", [[1, 280, Ease.out]]);
      this.anim("sy", [[0.95, 100, Ease.out], [1.0, 260, Ease.back]]);
      this.anim("sx", [[1.04, 100, Ease.out], [1.0, 260, Ease.back]]);
      Sound.play("greet");
    }, 250);

    setTimeout(() => { if (this.greetToken === tok) this.blink(); }, 550);
    setTimeout(() => { if (this.greetToken === tok) this.blink(); }, 1500);
    setTimeout(() => {
      if (this.greetToken !== tok) return;
      this.waveUntil = 0;
      this.anim("hands", [[0, 200, Ease.inOut]]);
    }, 1550);
    setTimeout(() => {
      if (this.greetToken !== tok) return;
      this.eyeOverride = "happy";
      this.eyeOverrideUntil = now() + 0.3;
    }, 1750);
  }

  interruptGreet() {
    if (this.hands <= 0.01 && now() >= this.waveUntil) return;
    this.greetToken++;
    this.waveUntil = 0;
    this.waveStart = 0;
    this.anim("hands", [[0, 150, Ease.inOut]]);
  }

  setPermanentEmote(emote: BotEmoteName | null) {
    this.permanentEmote = emote;
    if (emote === "wink") {
      this.miniNextBehavior = now() + 0.8 + Math.random() * 1.7;
      return;
    }
    this.permanentEye = emote ? EMOTE_EYE[emote] : null;
    if (this.permanentEye) {
      this.eyeOverride = this.permanentEye;
      this.eyeOverrideUntil = Number.POSITIVE_INFINITY;
    } else if (this.eyeOverrideUntil === Number.POSITIVE_INFINITY) {
      this.eyeOverride = null;
      this.eyeOverrideUntil = 0;
    }
    this.miniNextBehavior = now() + 0.8 + Math.random() * 1.7;
  }

  triggerEmote(emote: BotEmoteName, duration = 1.8) {
    const t = now();
    this.eyeOverride = EMOTE_EYE[emote];
    this.eyeOverrideUntil = t + duration;

    switch (emote) {
      case "love":
        this.anim("blush", [
          [1, 300, Ease.out], [1, (duration - 0.6) * 1000, Ease.lin], [0, 300, Ease.inOut],
        ]);
        this.emit("heart", 4);
        this.anim("oy", [[-0.1, 160, Ease.out], [0, 300, Ease.back]]);
        break;
      case "surprised":
        this.anim("oy", [[-0.3, 140, Ease.out], [0, 380, Ease.back]]);
        this.anim("es", [[1.25, 120, Ease.out], [1, 500, Ease.inOut]]);
        break;
      case "proud":
        this.emit("star", 5);
        this.anim("tilt", [
          [-0.14, 220, Ease.out], [-0.14, (duration - 0.5) * 1000, Ease.lin], [0, 280, Ease.inOut],
        ]);
        this.anim("blush", [
          [0.7, 250, Ease.out], [0.7, (duration - 0.5) * 1000, Ease.lin], [0, 300, Ease.inOut],
        ]);
        break;
      case "wink":
        this.anim("tilt", [
          [0.12, 160, Ease.out], [0.12, (duration - 0.4) * 1000, Ease.lin], [0, 240, Ease.inOut],
        ]);
        break;
      case "yawn":
        this.anim("sy", [[1.12, 500, Ease.inOut], [1, 500, Ease.inOut]]);
        this.anim("sx", [[0.94, 500, Ease.inOut], [1, 500, Ease.inOut]]);
        setTimeout(() => { this.eyeOverride = "closed"; this.emit("z", 2); }, 700);
        break;
      case "happy":
        this.anim("blush", [[0.6, 200, Ease.out], [0, 600, Ease.inOut]]);
        break;
      case "annoyed":
        this.eyeOverride = "line";
        this.eyeOverrideUntil = t + 0.8;
        setTimeout(() => Sound.play("annoyed"), 60);
        break;
    }
  }

  emit(type: Particle["type"], count: number) {
    for (let i = 0; i < count; i++) {
      const isZ = type === "z";
      this.particles.push({
        type,
        x: (Math.random() - 0.5) * 0.9 + (isZ ? 0.55 : 0),
        y: -0.7 - Math.random() * 0.2,
        vx: (Math.random() - 0.5) * 0.35 + (isZ ? 0.18 : 0),
        vy: -(0.45 + Math.random() * 0.35),
        age: -i * 0.14,
        life: 1.3 + Math.random() * 0.5,
        rot: Math.random() * Math.PI * 2,
        size: 0.15 + Math.random() * 0.08,
      });
    }
  }

  animateMorph(target: number, durationMs?: number) {
    const dur = durationMs ?? (target > 0.5 ? 550 : 650);
    this.anim("morph", [[target, dur, Ease.inOut]]);
  }

  resetMorph() {
    this.tweens.delete("morph");
    this.locks.delete("morph");
    this.morph = 0;
  }

  /** True while anything is still moving — lets the island stop its RAF loop. */
  get busy(): boolean {
    return (
      this.tweens.size > 0 ||
      this.particles.length > 0 ||
      this.cfg.bounces || this.cfg.scans || this.cfg.breathes || this.cfg.zz || this.cfg.sweat ||
      this.isMini ||
      Math.abs(this.tgYaw - this.yaw) > 0.002 ||
      Math.abs(this.tgPitch - this.pitch) > 0.002 ||
      Math.abs(this.tgTilt - this.tilt) > 0.002 ||
      Math.abs(this.tgSy - this.sy) > 0.002 ||
      Math.abs(this.tgSx - this.sx) > 0.002 ||
      Math.abs(this.tgEs - this.es) > 0.002 ||
      this.slotH > 0.001 || Math.abs(this.slotHVel) > 0.001 ||
      Math.abs(this.col[0] - this.colT[0]) > 0.003 ||
      Math.abs(this.col[1] - this.colT[1]) > 0.003 ||
      Math.abs(this.col[2] - this.colT[2]) > 0.003
    );
  }

  // ── Tweens ──────────────────────────────────────────────────────────────────

  anim(prop: PropKey, keys: TweenKey[], onComplete?: () => void) {
    this.tweens.set(prop, {
      prop, keys, index: 0, from: this[prop], startMs: performance.now(), onComplete,
    });
    this.locks.add(prop);
  }

  // ── Update ──────────────────────────────────────────────────────────────────

  update(dt: number) {
    const n = now();
    const nowMs = performance.now();

    for (const tw of [...this.tweens.values()]) {
      const k = tw.keys[tw.index];
      const p = Math.min(1, Math.max(0, (nowMs - tw.startMs) / k[1]));
      this[tw.prop] = tw.from + (k[0] - tw.from) * k[2](p);
      if (p >= 1) {
        tw.from = k[0];
        tw.index += 1;
        tw.startMs = nowMs;
        if (tw.index >= tw.keys.length) {
          this.tweens.delete(tw.prop);
          this.locks.delete(tw.prop);
          tw.onComplete?.();
        }
      }
    }

    const t = n - this.t0;
    let ty = this.lookX * 0.62;
    let tp = this.lookY * 0.5;

    if (this.cfg.look) {
      ty = ty * 0.35 + this.cfg.look[0] * 0.55;
      tp = tp * 0.3 + this.cfg.look[1] * 0.5;
    }
    if (this.cfg.scans) {
      ty = Math.sin(t * 2.6) * 0.6;
      tp = -0.06;
    }
    if (this.state === "sleeping") { ty = 0; tp = -0.14; }
    if (this.state === "dizzy") { ty = Math.sin(t * 9) * 0.25; }

    // Mini bots never follow the mouse — they wander.
    if (this.isMini && !this.cfg.look && !this.cfg.scans && this.state !== "sleeping" && this.state !== "dizzy") {
      if (n > this.miniLookNextTime) {
        this.miniLookTarget = {
          x: -0.88 + Math.random() * 1.76,
          y: -0.55 + Math.random() * 1.0,
        };
        this.miniLookNextTime = n + 0.5 + Math.random() * 1.5;
      }
      ty = this.miniLookTarget.x * 0.62;
      tp = this.miniLookTarget.y * 0.5;
    }

    this.tgYaw = ty;
    this.tgPitch = tp;
    this.tgTilt = this.cfg.tilt;

    if (n > this.waveStart && n < this.waveUntil) {
      const wt = n - this.waveStart;
      this.tgTilt = -0.06 + Math.sin(2 * Math.PI * 1.2 * wt) * 0.07;
    }

    const bounce = this.cfg.bounces ? -Math.abs(Math.sin(t * 5.2)) * 0.07 : 0;
    const kGen = 1 - Math.pow(0.0008, dt);
    if (!this.locks.has("oy")) this.oy += (bounce - this.oy) * kGen;

    if (this.cfg.breathes) {
      const amp = this.isMini ? 0.07 : 0.035;
      this.tgSy = 1 + Math.sin(t * 1.8) * amp;
      this.tgSx = 1 - Math.sin(t * 1.8) * amp * 0.57;
    } else if (this.isMini) {
      this.tgSy = 1 + Math.sin(t * 2.2) * 0.04;
      this.tgSx = 1 - Math.sin(t * 2.2) * 0.02;
    } else {
      this.tgSy = 1;
      this.tgSx = 1;
    }

    if (this.isMini && n > this.miniNextBehavior) this.doMiniBehaviorLoop();

    const kLook = 1 - Math.pow(0.0025, dt);
    if (!this.locks.has("yaw")) this.yaw += (this.tgYaw - this.yaw) * kLook;
    if (!this.locks.has("pitch")) this.pitch += (this.tgPitch - this.pitch) * kLook;
    if (!this.locks.has("tilt")) this.tilt += (this.tgTilt - this.tilt) * kGen;
    if (!this.locks.has("sy")) this.sy += (this.tgSy - this.sy) * kGen;
    if (!this.locks.has("sx")) this.sx += (this.tgSx - this.sx) * kGen;
    if (!this.locks.has("es")) this.es += (this.tgEs - this.es) * kGen;

    this.col = mix3(this.col, this.colT, 1 - Math.pow(0.002, dt));

    if (n > this.nextBlink) {
      if (this.state !== "sleeping" && this.state !== "dizzy") {
        this.blink();
        if (Math.random() < 0.22) setTimeout(() => this.blink(), 230);
      }
      this.nextBlink = n + 2.2 + Math.random() * 3.2;
    }

    if (this.eyeOverride && n > this.eyeOverrideUntil) {
      this.eyeOverride = this.permanentEye;
      if (this.permanentEye) this.eyeOverrideUntil = Number.POSITIVE_INFINITY;
    }

    if (n - this.lastAmbient > 1.3) {
      this.lastAmbient = n;
      if (this.cfg.zz) this.emit("z", 1);
      if (!this.isMini && this.cfg.sweat && Math.random() < 0.5) this.emit("sweat", 1);
    }

    for (const p of this.particles) p.age += dt;
    this.particles = this.particles.filter((p) => p.age < p.life);

    // Mouth slot spring — ω₀ = 2π/0.25, ζ = 0.6
    const omega = (2 * Math.PI) / 0.25;
    const zeta = 0.6;
    const acc = omega * omega * (this.slotHTarget - this.slotH) - 2 * zeta * omega * this.slotHVel;
    this.slotHVel += acc * dt;
    this.slotH = Math.max(0, this.slotH + this.slotHVel * dt);

    this.lastTime = n;
  }

  private doMiniBehaviorLoop() {
    const n = now();
    switch (this.permanentEmote) {
      case "happy":
        if (this.locks.has("oy")) { this.miniNextBehavior = n + 0.4; return; }
        this.anim("oy", [[-0.3, 120, Ease.out], [0.03, 200, Ease.inOut], [0, 160, Ease.back]]);
        this.anim("sy", [[0.82, 80, Ease.out], [1.18, 130, Ease.out], [0.88, 160, Ease.inOut], [1, 200, Ease.back]]);
        this.anim("sx", [[1.15, 80, Ease.out], [0.88, 130, Ease.out], [1.06, 160, Ease.inOut], [1, 200, Ease.back]]);
        this.miniNextBehavior = n + 2.2 + Math.random() * 1.2;
        break;
      case "annoyed":
        if (this.locks.has("yaw")) { this.miniNextBehavior = n + 0.5; return; }
        this.anim("yaw", [
          [-0.65, 50, Ease.out], [0.65, 90, Ease.inOut], [-0.5, 80, Ease.inOut],
          [0.4, 75, Ease.inOut], [-0.2, 70, Ease.inOut], [0, 140, Ease.out],
        ]);
        this.miniNextBehavior = n + 3.0 + Math.random() * 2.5;
        break;
      case "wink":
        this.eyeOverride = "wink";
        this.eyeOverrideUntil = n + 0.55;
        this.anim("tilt", [[0.13, 100, Ease.out], [0.13, 320, Ease.lin], [0, 200, Ease.inOut]]);
        this.miniNextBehavior = n + 2.2 + Math.random() * 2.0;
        break;
      case "love":
        this.emit("heart", 2);
        this.anim("tilt", [[-0.1, 180, Ease.out], [0.1, 340, Ease.inOut], [0, 220, Ease.inOut]]);
        this.miniNextBehavior = n + 2.6 + Math.random() * 1.5;
        break;
      default:
        this.miniNextBehavior = n + 3.0 + Math.random() * 2.0;
    }
  }


  // ── Draw ────────────────────────────────────────────────────────────────────

  /** Draws Bob into a canvas of `W`×`H` CSS pixels (the caller applied the DPR transform). */
  draw(x: CanvasRenderingContext2D, W: number, H: number) {
    const R = W * 0.3;
    this.drawAt(x, W / 2 + this.ox * R, H / 2 + this.particleOverhang / 2 + this.oy * R + R * 0.06, R);
  }

  /** Draws Bob centred on (cx, cy) with body half-size ≈ R. */
  drawAt(x: CanvasRenderingContext2D, cx: number, cy: number, R: number) {
    const m = this.morph;
    const hw = lerp(R * 1.0, R * 1.2, m);
    const hh = lerp(R * 0.96, R * 0.88, m);
    const rad = lerp(R * 0.52, R * 0.3, m);

    this.drawHands(x, R, hw, hh, cx, cy);

    x.save();
    x.translate(cx, cy);
    if (this.tilt !== 0) x.rotate(this.tilt);
    x.scale(this.sx, this.sy);

    // The amber twin peeks out behind him (hidden once he turns into the box).
    if (m < 0.9) {
      x.save();
      x.globalAlpha = 1 - m;
      x.translate(R * 0.17, R * 0.15);
      x.rotate(0.07);
      roundRectPath(x, -hw * 0.97, -hh * 0.97, hw * 1.94, hh * 1.94, rad);
      const tg = x.createLinearGradient(-hw, -hh, hw, hh);
      tg.addColorStop(0, rgba(AMBER));
      tg.addColorStop(1, rgba(AMBER_DEEP));
      x.fillStyle = tg;
      x.fill();
      x.restore();
    }

    this.drawAntenna(x, R, hh, rad);

    const body = new Path2D();
    pathRoundRect(body, -hw, -hh, hw * 2, hh * 2, rad);
    this.drawBody(x, body, R, hw, hh);

    // Cheeks
    const blushVal = Math.max(this.blush, this.tint * 0.25) * (1 - m);
    const cheekA = 0.16 + 0.5 * blushVal;
    x.save();
    x.clip(body);
    x.fillStyle = `rgba(255,120,140,${cheekA})`;
    const slide = Math.sin(this.yaw) * hw * 0.35;
    for (const sd of [-1, 1]) {
      x.beginPath();
      x.ellipse(sd * hw * 0.62 + slide, hh * 0.3, R * 0.15, R * 0.09, 0, 0, Math.PI * 2);
      x.fill();
    }
    x.restore();

    this.drawEyes(x, body, R, hw, hh);
    if (m > 0.05) this.drawSlot(x, body, R, hh);
    else this.drawMouth(x, R, hh);

    x.restore();

    if (this.badge && this.badgeS > 0.01 && m < 0.25) this.drawBadge(x, this.badge, R, cx, cy);
    this.drawParticles(x, R, cx, cy);
  }

  private drawBody(x: CanvasRenderingContext2D, body: Path2D, R: number, hw: number, hh: number) {
    const g = x.createLinearGradient(-hw * 0.8, -hh, hw * 0.8, hh);
    const tint = this.tint * (1 - this.morph);
    g.addColorStop(0, rgba(mix3(TEAL_TOP, this.col, tint * 0.4)));
    g.addColorStop(1, rgba(mix3(TEAL_BOTTOM, this.col, tint * 0.55)));
    x.fillStyle = g;
    x.fill(body);

    // Soft inner shade at the bottom, glossy highlight at the top-left.
    const sh = x.createLinearGradient(0, hh * 0.2, 0, hh);
    sh.addColorStop(0, "rgba(0,40,45,0)");
    sh.addColorStop(1, "rgba(0,40,45,0.28)");
    x.fillStyle = sh;
    x.fill(body);

    x.save();
    x.clip(body);
    const hl = x.createRadialGradient(-hw * 0.4, -hh * 0.55, 0, -hw * 0.4, -hh * 0.55, R * 0.7);
    hl.addColorStop(0, "rgba(255,255,255,0.6)");
    hl.addColorStop(1, "rgba(255,255,255,0)");
    x.fillStyle = hl;
    x.fillRect(-hw, -hh, hw * 2, hh * 2);
    x.restore();

    x.lineWidth = Math.max(1, R * 0.03);
    x.strokeStyle = "rgba(255,255,255,0.22)";
    x.stroke(body);
  }

  /** Antenna: a short stalk and a bulb that glows in the state's colour. */
  private drawAntenna(x: CanvasRenderingContext2D, R: number, hh: number, rad: number) {
    const a = 1 - this.morph;
    if (a < 0.05 || R < 12) return;
    const t = now();
    const sway = Math.sin(t * 2.2) * 0.04 + this.yaw * 0.12;
    const len = R * 0.34;
    const topY = -hh + rad * 0.12;
    const bx = Math.sin(sway) * len;
    const by = topY - Math.cos(sway) * len;

    x.save();
    x.globalAlpha = a;
    x.strokeStyle = rgba(mix3(TEAL_BOTTOM, [0, 0.15, 0.2], 0.35));
    x.lineWidth = Math.max(1.5, R * 0.06);
    x.lineCap = "round";
    x.beginPath();
    x.moveTo(0, topY + R * 0.04);
    x.quadraticCurveTo(bx * 0.2, topY - len * 0.6, bx, by);
    x.stroke();

    const pulse = this.cfg.bounces || this.cfg.sweat ? 0.5 + 0.5 * Math.sin(t * 6) : 0.5 + 0.5 * Math.sin(t * 2);
    const br = R * 0.11;
    const glow = x.createRadialGradient(bx, by, 0, bx, by, br * (2.6 + pulse));
    glow.addColorStop(0, rgba(this.col, 0.55));
    glow.addColorStop(1, rgba(this.col, 0));
    x.fillStyle = glow;
    x.beginPath();
    x.arc(bx, by, br * (2.6 + pulse), 0, Math.PI * 2);
    x.fill();
    x.fillStyle = rgba(mix3(this.col, [1, 1, 1], 0.15));
    x.beginPath();
    x.arc(bx, by, br, 0, Math.PI * 2);
    x.fill();
    x.fillStyle = "rgba(255,255,255,0.8)";
    x.beginPath();
    x.arc(bx - br * 0.3, by - br * 0.3, br * 0.32, 0, Math.PI * 2);
    x.fill();
    x.restore();
  }

  private drawEyes(x: CanvasRenderingContext2D, body: Path2D, R: number, hw: number, hh: number) {
    let shape: EyeShape = this.eyeOverride ?? this.cfg.eye;
    if (this.morph > 0.5) {
      if (this.isChewing) shape = "happy";
      else if (this.slotHTarget > 0.05 || this.slotH > 0.1) shape = "cup";
    }
    x.save();
    x.clip(body);
    x.fillStyle = INK;
    x.strokeStyle = INK;
    const lift = this.morph * hh * 0.1;
    for (const sd of [-1, 1]) {
      const ex = sd * hw * 0.4 + this.yaw * hw * 0.42 + Math.sin(this.roll) * hw * 0.2;
      const ey = -hh * 0.1 + this.pitch * hh * 0.55 + lift;
      // Eyes squeeze a little as they look sideways, like they sit on a curved face.
      const fx = 1 - Math.min(0.28, Math.abs(this.yaw) * 0.35);
      x.save();
      x.translate(ex, ey);
      x.scale(fx, 1);
      this.drawEyeShape(x, shape, R * 0.19 * this.es, R * 0.27 * this.es, sd);
      x.restore();
    }
    x.restore();
  }

  private drawEyeShape(x: CanvasRenderingContext2D, shape: EyeShape, w: number, h: number, sd: number) {
    const t = now();
    const arc = (up: boolean) => {
      x.lineWidth = w * 0.42;
      x.lineCap = "round";
      x.beginPath();
      if (up) x.arc(0, h * 0.22, w * 0.78, Math.PI * 1.1, Math.PI * 1.9);
      else x.arc(0, -h * 0.12, w * 0.78, Math.PI * 0.1, Math.PI * 0.9);
      x.stroke();
    };
    const glossy = (ew: number, eh: number) => {
      const hh = Math.max(eh * this.open, ew * 0.35);
      x.beginPath();
      x.ellipse(0, 0, ew, hh, 0, 0, Math.PI * 2);
      x.fill();
      if (hh > ew * 0.7) {
        x.fillStyle = "rgba(255,255,255,0.92)";
        x.beginPath();
        x.arc(-ew * 0.32, -hh * 0.36, ew * 0.34, 0, Math.PI * 2);
        x.fill();
        x.beginPath();
        x.arc(ew * 0.34, hh * 0.3, ew * 0.15, 0, Math.PI * 2);
        x.fill();
        x.fillStyle = INK;
      }
    };
    switch (shape) {
      case "pill":
        glossy(w * 0.62, h * 0.9);
        break;
      case "wide":
        glossy(w * 0.75, h * 1.05);
        break;
      case "dot":
        glossy(w * 0.62, w * 0.62);
        break;
      case "line":
        x.rotate(-sd * 0.22);
        roundRectPath(x, -w * 0.8, -w * 0.18, w * 1.6, w * 0.36, w * 0.18);
        x.fill();
        break;
      case "flat":
        x.rotate(sd * 0.16);
        roundRectPath(x, -w * 0.78, -w * 0.17, w * 1.56, w * 0.34, w * 0.17);
        x.fill();
        break;
      case "happy":
        arc(true);
        break;
      case "closed":
        arc(false);
        break;
      case "spiral": {
        x.lineWidth = w * 0.22;
        x.lineCap = "round";
        x.beginPath();
        for (let a = 0; a < 4.4 * Math.PI; a += 0.2) {
          const r = w * 0.06 + a * w * 0.058;
          const aa = a + t * 9 * sd;
          const px = Math.cos(aa) * r;
          const py = Math.sin(aa) * r;
          if (a === 0) x.moveTo(px, py);
          else x.lineTo(px, py);
        }
        x.stroke();
        break;
      }
      case "heart":
        x.fillStyle = "#FF4D6D";
        heartPath(x, w * 1.15);
        x.fill();
        x.fillStyle = INK;
        break;
      case "star":
        x.fillStyle = "#FFC933";
        x.rotate(t * 1.5 * sd);
        starPath(x, w * 1.05, w * 0.46);
        x.fill();
        x.fillStyle = INK;
        break;
      case "tired": {
        // Heavy-lidded: only the lower half of the eye shows, under a flat lid line.
        x.beginPath();
        x.ellipse(0, 0, w * 0.62, h * 0.62, 0, 0, Math.PI);
        x.closePath();
        x.fill();
        break;
      }
      case "wink":
        if (sd < 0) glossy(w * 0.62, h * 0.9);
        else arc(true);
        break;
      case "cup": {
        const hh = Math.max(h * this.open * 0.9, w * 0.35);
        x.beginPath();
        x.ellipse(0, 0, w * 0.62, hh, 0, 0, Math.PI * 2);
        x.fill();
        break;
      }
    }
  }

  /** Mouth for the normal (cube) body. */
  private drawMouth(x: CanvasRenderingContext2D, R: number, hh: number) {
    let shape: MouthShape = this.cfg.mouth;
    const eye = this.eyeOverride;
    if (eye === "happy" || eye === "heart" || eye === "star") shape = "grin";
    else if (eye === "dot") shape = "o";
    else if (eye === "line") shape = "flat";
    else if (eye === "tired" || eye === "closed") shape = "tiny";
    else if (eye === "wink") shape = "smile";

    const y = hh * 0.42 + this.pitch * hh * 0.15;
    const mx = this.yaw * R * 0.3;
    x.save();
    x.translate(mx, y);
    x.strokeStyle = INK;
    x.fillStyle = INK;
    x.lineCap = "round";
    x.lineJoin = "round";
    x.lineWidth = Math.max(1.4, R * 0.07);
    const w = R * 0.26;
    switch (shape) {
      case "smile":
        x.beginPath();
        x.moveTo(-w, 0);
        x.quadraticCurveTo(0, R * 0.2, w, 0);
        x.stroke();
        break;
      case "grin":
        x.beginPath();
        x.moveTo(-w * 1.15, -R * 0.02);
        x.quadraticCurveTo(0, R * 0.32, w * 1.15, -R * 0.02);
        x.closePath();
        x.fill();
        x.fillStyle = "rgba(255,120,140,0.9)";
        x.beginPath();
        x.ellipse(0, R * 0.12, w * 0.5, R * 0.06, 0, 0, Math.PI * 2);
        x.fill();
        break;
      case "flat":
        x.beginPath();
        x.moveTo(-w * 0.8, R * 0.04);
        x.lineTo(w * 0.8, R * 0.04);
        x.stroke();
        break;
      case "o":
        x.beginPath();
        x.ellipse(0, R * 0.05, R * 0.09, R * 0.12, 0, 0, Math.PI * 2);
        x.fill();
        break;
      case "wavy": {
        x.beginPath();
        const t = now();
        for (let i = 0; i <= 12; i++) {
          const u = i / 12;
          const px = -w + u * w * 2;
          const py = Math.sin(u * Math.PI * 3 + t * 6) * R * 0.04;
          if (i === 0) x.moveTo(px, py);
          else x.lineTo(px, py);
        }
        x.stroke();
        break;
      }
      case "tiny":
        x.beginPath();
        x.moveTo(-w * 0.4, 0);
        x.quadraticCurveTo(0, R * 0.07, w * 0.4, 0);
        x.stroke();
        break;
      case "hmm":
        x.beginPath();
        x.moveTo(-w * 0.8, R * 0.05);
        x.quadraticCurveTo(-w * 0.1, -R * 0.02, w * 0.8, R * 0.06);
        x.stroke();
        break;
    }
    x.restore();
  }

  /** The cube's big "eating" mouth: a dark slot with an amber rim, used for drops. */
  private drawSlot(x: CanvasRenderingContext2D, body: Path2D, R: number, hh: number) {
    const m = this.morph;
    const w = R * 1.55 * m;
    const h = Math.max(R * 0.1, this.slotH * R * 1.1) * m;
    const y = hh * 0.2;
    x.save();
    x.clip(body);
    const r = Math.min(w / 2, h / 2);
    roundRectPath(x, -w / 2, y, w, h, r);
    const g = x.createLinearGradient(0, y, 0, y + h);
    g.addColorStop(0, "rgb(6,18,22)");
    g.addColorStop(1, "rgb(14,40,46)");
    x.fillStyle = g;
    x.fill();
    x.lineWidth = Math.max(1.2, R * 0.05);
    x.strokeStyle = `rgba(251,191,36,${0.9 * m})`;
    x.stroke();
    x.restore();
  }

  /** Little round hands, in the amber of the twin; they wave during the greeting. */
  private drawHands(x: CanvasRenderingContext2D, R: number, hw: number, hh: number, cx: number, cy: number) {
    if (this.hands <= 0.01 || R <= 14) return;
    const n = now();
    const r = R * 0.2 * this.hands;
    const waving = this.waveStart > 0 && n >= this.waveStart && n < this.waveUntil;
    const cosT = Math.cos(this.tilt);
    const sinT = Math.sin(this.tilt);
    for (const sd of [-1, 1]) {
      let lx = sd * hw * this.sx * 1.04;
      let ly = hh * this.sy * 0.45;
      if (sd > 0 && waving) {
        const wt = n - this.waveStart;
        const rise = 1 - Math.pow(1 - Math.min(1, wt / 0.18), 3);
        lx += (hw * 0.18 + Math.cos(13 * wt) * R * 0.1) * rise;
        ly += (-hh * 1.15 + Math.sin(13 * wt) * R * 0.2) * rise;
      }
      const wx = cx + cosT * lx - sinT * ly;
      const wy = cy + sinT * lx + cosT * ly;
      const g = x.createRadialGradient(wx - r * 0.3, wy - r * 0.3, 0, wx, wy, r * 1.1);
      g.addColorStop(0, rgba(mix3(AMBER, [1, 1, 1], 0.4)));
      g.addColorStop(1, rgba(AMBER_DEEP));
      x.beginPath();
      x.arc(wx, wy, r, 0, Math.PI * 2);
      x.fillStyle = g;
      x.fill();
    }
  }

  private drawBadge(x: CanvasRenderingContext2D, badge: Badge, R: number, cx: number, cy: number) {
    const bs = this.badgeS;
    const bx = cx - R * 0.78 * this.sx;
    const by = cy - R * 0.78 * this.sy;
    const t = now();
    x.save();
    x.translate(bx, by);
    x.scale(bs, bs);
    const col = rgba(badge.color);
    if (badge.kind === "dots") {
      const pw = R * 0.74;
      const ph = R * 0.38;
      roundRectPath(x, -pw / 2, -ph / 2, pw, ph, ph * 0.38);
      x.fillStyle = col;
      x.fill();
      for (let i = 0; i < 3; i++) {
        const phase = (((t * 2.4 - i * 0.22) % 1) + 1) % 1;
        const dotR = R * 0.055 * (1 + 0.4 * Math.max(0, Math.sin(phase * Math.PI * 2)));
        x.fillStyle = "#fff";
        x.beginPath();
        x.arc((i - 1) * R * 0.18, 0, dotR, 0, Math.PI * 2);
        x.fill();
      }
    } else if (badge.kind === "bang" || badge.kind === "question") {
      const s = R * 0.5;
      roundRectPath(x, -s / 2, -s / 2, s, s, s * 0.34);
      x.fillStyle = col;
      x.fill();
      x.fillStyle = "#fff";
      x.font = `900 ${R * 0.32}px ${FONT}`;
      x.textAlign = "center";
      x.textBaseline = "middle";
      x.fillText(badge.kind === "bang" ? "!" : "?", 0, R * 0.02);
    } else {
      x.fillStyle = "#06141a";
      x.beginPath();
      x.arc(0, 0, R * 0.2, 0, Math.PI * 2);
      x.fill();
      x.fillStyle = col;
      x.beginPath();
      x.arc(0, 0, R * 0.135, 0, Math.PI * 2);
      x.fill();
    }
    x.restore();
  }

  private drawParticles(x: CanvasRenderingContext2D, R: number, cx: number, cy: number) {
    for (const p of this.particles) {
      if (p.age <= 0) continue;
      const k = p.age / p.life;
      const a = k < 0.2 ? k / 0.2 : 1 - (k - 0.2) / 0.8;
      const px = cx + (p.x + p.vx * p.age) * R * 1.3;
      const py = cy + (p.y + p.vy * p.age) * R * 1.3;
      const sz = R * p.size * (1 + k * 0.4);
      x.save();
      x.translate(px, py);
      x.globalAlpha = Math.min(1, Math.max(0, a));
      switch (p.type) {
        case "heart":
          x.rotate(Math.sin(p.age * 6) * 0.3);
          x.fillStyle = "#FF4D6D";
          heartPath(x, sz);
          x.fill();
          break;
        case "star":
          x.rotate(p.rot + p.age * 2);
          x.fillStyle = "#FFC933";
          starPath(x, sz, sz * 0.45);
          x.fill();
          break;
        case "spark":
          x.rotate(p.rot);
          x.fillStyle = "#fff";
          starPath(x, sz * 0.8, sz * 0.18);
          x.fill();
          break;
        case "sweat":
          x.fillStyle = "#7CC7FF";
          x.beginPath();
          x.moveTo(0, -sz);
          x.quadraticCurveTo(sz * 0.8, sz * 0.2, 0, sz * 0.6);
          x.quadraticCurveTo(-sz * 0.8, sz * 0.2, 0, -sz);
          x.fill();
          break;
        case "z":
          x.fillStyle = "rgb(190,235,230)";
          x.font = `700 ${sz * 1.9}px ${FONT}`;
          x.textAlign = "center";
          x.textBaseline = "middle";
          x.fillText("z", 0, 0);
          break;
      }
      x.restore();
    }
  }
}

function pathRoundRect(p: Path2D, X: number, Y: number, W: number, H: number, R: number) {
  const r = Math.max(0, Math.min(R, W / 2, H / 2));
  p.moveTo(X + r, Y);
  p.arcTo(X + W, Y, X + W, Y + H, r);
  p.arcTo(X + W, Y + H, X, Y + H, r);
  p.arcTo(X, Y + H, X, Y, r);
  p.arcTo(X, Y, X + W, Y, r);
  p.closePath();
}
