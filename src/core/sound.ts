// Tandem's sounds are synthesised in code — no audio files. Each sound is a short
// list of notes (with optional pitch glide) or a noise sweep, rendered once into an
// AudioBuffer when the island starts. Default volume 0.12, slider range 0–0.2.

import { invoke } from "@tauri-apps/api/core";
import { IS_TAURI } from "../bridge";

export const SOUND_NAMES = [
  "peek", "open", "close", "hover", "blip", "slap", "annoyed", "dizzy", "greet",
  "work", "finish", "error", "approval", "question", "approve", "gulp", "tick",
  "send", "love", "pop", "proud", "wink", "yawn", "attach", "think", "search",
  "rate", "sleep",
  // The device moments and Bob's play: soft, physical, never a "ding".
  "plug", "unplug", "low", "full", "hello", "bye", "boop", "giggle", "purr", "whoosh", "tink",
] as const;

export type SoundName = (typeof SOUND_NAMES)[number];

type Wave = "sine" | "tri" | "square" | "saw";
/** [start s, duration s, from Hz, to Hz, volume, wave, vibrato Hz?] */
type Note = readonly [number, number, number, number, number, Wave?, number?];
/** Noise burst: [start, duration, low-pass start Hz, low-pass end Hz, volume] */
type Noise = readonly [number, number, number, number, number];
interface Spec { notes?: Note[]; noise?: Noise[] }

const n = (start: number, dur: number, f: number, v = 0.5, wave: Wave = "sine", to = f, vib = 0): Note => [start, dur, f, to, v, wave, vib];

const SPECS: Record<SoundName, Spec> = {
  peek: { notes: [n(0, 0.09, 523, 0.4), n(0.07, 0.14, 784, 0.4)] },
  open: { notes: [n(0, 0.16, 440, 0.45, "sine", 740), n(0.05, 0.2, 880, 0.18, "sine", 1480)] },
  close: { notes: [n(0, 0.16, 740, 0.4, "sine", 440)] },
  hover: { notes: [n(0, 0.04, 1500, 0.22, "sine", 1800)] },
  blip: { notes: [n(0, 0.07, 880, 0.4, "tri")] },
  slap: { notes: [n(0, 0.1, 160, 0.7, "sine", 70)], noise: [[0, 0.07, 3500, 800, 0.5]] },
  annoyed: { notes: [n(0, 0.12, 320, 0.35, "saw", 250), n(0.14, 0.16, 270, 0.35, "saw", 200)] },
  dizzy: { notes: [n(0, 0.7, 420, 0.4, "sine", 300, 9), n(0.05, 0.65, 630, 0.15, "sine", 450, 11)] },
  greet: { notes: [n(0, 0.1, 784, 0.4), n(0.1, 0.1, 988, 0.4), n(0.2, 0.28, 1319, 0.4)] },
  work: { notes: [n(0, 0.05, 620, 0.3, "tri"), n(0.09, 0.05, 700, 0.3, "tri")] },
  finish: { notes: [n(0, 0.12, 523, 0.4), n(0.1, 0.12, 659, 0.4), n(0.2, 0.12, 784, 0.4), n(0.3, 0.4, 1047, 0.45)] },
  // Two soft falling notes: says "that didn't work" without a buzzer.
  error: { notes: [n(0, 0.14, 466, 0.32, "tri", 415), n(0.16, 0.24, 349, 0.32, "sine", 311)] },
  approval: { notes: [n(0, 0.12, 659, 0.42), n(0.13, 0.22, 880, 0.42)] },
  question: { notes: [n(0, 0.1, 494, 0.4), n(0.1, 0.22, 740, 0.42, "sine", 830)] },
  approve: { notes: [n(0, 0.08, 659, 0.42), n(0.08, 0.16, 988, 0.42)] },
  gulp: { notes: [n(0, 0.16, 320, 0.5, "sine", 110), n(0.2, 0.1, 200, 0.35, "sine", 330), n(0.3, 0.12, 330, 0.3, "sine", 140)] },
  tick: { notes: [n(0, 0.025, 1700, 0.3, "square")] },
  send: { notes: [n(0, 0.28, 380, 0.4, "sine", 1250)], noise: [[0, 0.3, 600, 5000, 0.18]] },
  love: { notes: [n(0, 0.2, 784, 0.35, "sine", 784, 6), n(0.14, 0.22, 988, 0.35, "sine", 988, 6), n(0.28, 0.4, 1175, 0.38, "sine", 1175, 6)] },
  pop: { notes: [n(0, 0.07, 480, 0.5, "sine", 960)] },
  proud: { notes: [n(0, 0.16, 523, 0.38), n(0.15, 0.16, 659, 0.38), n(0.3, 0.45, 784, 0.42), n(0.3, 0.45, 392, 0.2)] },
  wink: { notes: [n(0, 0.05, 1318, 0.35, "tri"), n(0.06, 0.09, 1760, 0.35, "tri")] },
  yawn: { notes: [n(0, 0.7, 330, 0.35, "sine", 170)], noise: [[0.05, 0.6, 900, 300, 0.07]] },
  attach: { notes: [n(0, 0.06, 700, 0.35, "tri", 760), n(0.09, 0.09, 1000, 0.35, "tri", 1100)] },
  think: { notes: [n(0, 0.12, 440, 0.3, "sine", 440, 5), n(0.18, 0.14, 466, 0.3, "sine", 466, 5)] },
  search: { notes: [n(0, 0.28, 500, 0.32, "sine", 940), n(0.28, 0.28, 940, 0.28, "sine", 500)] },
  rate: { notes: [n(0, 0.16, 349, 0.32, "tri", 311), n(0.18, 0.24, 294, 0.32, "tri", 262)] },
  sleep: { notes: [n(0, 0.9, 392, 0.3, "sine", 262), n(0.05, 0.85, 196, 0.15, "sine", 131)] },
  // A felt "thunk" as the charger clicks in, then a soft shimmer rising out of it.
  plug: {
    notes: [n(0, 0.14, 150, 0.7, "sine", 70), n(0.1, 0.5, 1568, 0.09, "sine", 1568, 5), n(0.16, 0.5, 2093, 0.08, "sine", 2093, 5), n(0.22, 0.6, 2637, 0.07, "sine", 2637, 4)],
    noise: [[0, 0.05, 1800, 400, 0.22]],
  },
  // Let go of the charger: a gentle drop.
  unplug: { notes: [n(0, 0.2, 620, 0.32, "sine", 330), n(0.03, 0.14, 140, 0.4, "sine", 90)], noise: [[0, 0.04, 1200, 300, 0.12]] },
  // Tired, not alarming: two soft falling notes.
  low: { notes: [n(0, 0.22, 587, 0.3, "tri", 560), n(0.2, 0.36, 440, 0.3, "tri", 415)] },
  // Full: a little sparkle.
  full: { notes: [n(0, 0.1, 1319, 0.22), n(0.07, 0.1, 1568, 0.22), n(0.14, 0.12, 1976, 0.22), n(0.21, 0.45, 2637, 0.2, "sine", 2637, 6)] },
  // A computer joins: a warm "hey", with a breath of air.
  hello: { notes: [n(0, 0.14, 523, 0.34, "sine", 540), n(0.12, 0.32, 784, 0.32, "sine", 784, 4)], noise: [[0, 0.18, 500, 2500, 0.05]] },
  bye: { notes: [n(0, 0.16, 784, 0.3, "sine", 760), n(0.14, 0.34, 523, 0.28, "sine", 500)] },
  boop: { notes: [n(0, 0.08, 420, 0.5, "sine", 980), n(0.07, 0.06, 1250, 0.18, "sine", 1400)] },
  giggle: { notes: [n(0, 0.06, 880, 0.3, "sine", 940, 12), n(0.08, 0.06, 990, 0.3, "sine", 1060, 12), n(0.16, 0.09, 1110, 0.3, "sine", 1200, 12)] },
  // Being petted: a low, soft rumble.
  purr: { notes: [n(0, 0.9, 92, 0.5, "sine", 88, 24), n(0, 0.9, 184, 0.12, "sine", 176, 24)], noise: [[0, 0.9, 260, 180, 0.1]] },
  // Crossing the edge: air moving past.
  whoosh: { noise: [[0, 0.28, 300, 2600, 0.5]], notes: [n(0, 0.24, 220, 0.06, "sine", 440)] },
  tink: { notes: [n(0, 0.18, 2637, 0.26, "sine", 2637), n(0.01, 0.12, 3951, 0.1)] },
};

const RATE = 44100;
/** The slider tops out at 0.2; the sounds are normalised, so scale it up to a comfortable level. */
const MASTER_BOOST = 3;

function wave(kind: Wave, ph: number): number {
  const f = ph - Math.floor(ph);
  switch (kind) {
    case "tri": return 4 * Math.abs(f - 0.5) - 1;
    case "square": return f < 0.5 ? 0.6 : -0.6;
    case "saw": return (2 * f - 1) * 0.6;
    default: return Math.sin(ph * Math.PI * 2);
  }
}

/** Renders one sound spec to mono samples in −1…1. */
function render(spec: Spec): Float32Array {
  let end = 0;
  for (const [s, d] of spec.notes ?? []) end = Math.max(end, s + d);
  for (const [s, d] of spec.noise ?? []) end = Math.max(end, s + d);
  const out = new Float32Array(Math.ceil((end + 0.05) * RATE));

  for (const [start, dur, f0, f1, vol, kind = "sine", vib = 0] of spec.notes ?? []) {
    const i0 = Math.floor(start * RATE);
    const len = Math.floor(dur * RATE);
    let ph = 0;
    for (let i = 0; i < len; i++) {
      const u = i / len;
      let f = f0 + (f1 - f0) * u;
      if (vib) f *= 1 + 0.03 * Math.sin((i / RATE) * vib * Math.PI * 2);
      ph += f / RATE;
      // Fast attack, smooth exponential-ish release: no clicks.
      const env = Math.min(1, i / (0.006 * RATE)) * Math.pow(1 - u, 1.6);
      out[i0 + i] += wave(kind, ph) * vol * env;
    }
  }

  let seed = 12345;
  const rnd = () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 4294967296 * 2 - 1;
  };
  for (const [start, dur, lp0, lp1, vol] of spec.noise ?? []) {
    const i0 = Math.floor(start * RATE);
    const len = Math.floor(dur * RATE);
    let y = 0;
    for (let i = 0; i < len; i++) {
      const u = i / len;
      const fc = lp0 + (lp1 - lp0) * u;
      const a = 1 - Math.exp((-2 * Math.PI * fc) / RATE);
      y += a * (rnd() - y);
      out[i0 + i] += y * vol * Math.min(1, i / (0.004 * RATE)) * Math.pow(1 - u, 1.4);
    }
  }
  // Every sound is brought to the same peak, so the volume slider means the same for all.
  let peak = 0;
  for (let i = 0; i < out.length; i++) peak = Math.max(peak, Math.abs(out[i]));
  const gain = peak > 0 ? 0.9 / peak : 1;
  for (let i = 0; i < out.length; i++) out[i] = Math.max(-1, Math.min(1, out[i] * gain));
  return out;
}

class SoundEngine {
  enabled = true;
  volume = 0.12;

  private ctx: AudioContext | null = null;
  private master: GainNode | null = null;
  private buffers = new Map<string, AudioBuffer>();
  private loading: Promise<void> | null = null;
  /** True once Windows itself plays the sounds (no WebView audio involved). */
  private native = false;
  private idleTimer: number | null = null;

  /** Creates the context and decodes every WAV. Safe to call more than once. */
  preload(): Promise<void> {
    if (this.loading) return this.loading;
    this.loading = (async () => {
      if (IS_TAURI && (await this.registerNative())) return;
      const Ctor = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
      if (!Ctor) return;
      const ctx = new Ctor({ sampleRate: RATE });
      this.ctx = ctx;
      const master = ctx.createGain();
      master.gain.value = this.volume * MASTER_BOOST;
      master.connect(ctx.destination);
      this.master = master;
      for (const name of SOUND_NAMES) {
        const data = render(SPECS[name]);
        const buf = ctx.createBuffer(1, data.length, RATE);
        buf.getChannelData(0).set(data);
        this.buffers.set(name, buf);
      }
    })();
    return this.loading;
  }

  /** Hands every rendered sound to the Rust side, which plays them with the OS. */
  private async registerNative(): Promise<boolean> {
    // Only Windows plays them natively; the Mac's WebView plays them itself.
    if (!/Windows/i.test(navigator.userAgent)) return false;
    try {
      for (const name of SOUND_NAMES) {
        const data = render(SPECS[name]);
        const pcm = new Int16Array(data.length);
        for (let i = 0; i < data.length; i++) pcm[i] = Math.round(data[i] * 32767);
        await invoke("sound_register", pcm.buffer, { headers: { "x-sound": name } });
      }
      this.native = true;
      return true;
    } catch (err) {
      console.error("[tandem] native sound unavailable", err);
      return false;
    }
  }

  /** WebView2 can hand us a suspended context; call after any user input. */
  resume() {
    if (this.idleTimer != null) {
      window.clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
    void this.ctx?.resume();
  }

  /**
   * Called when the island goes quiet. A running AudioContext keeps an audio
   * thread and its render quantum alive even with nothing playing, which shows
   * up as a steady trickle of CPU on a machine that is supposed to be idle.
   *
   * The delay covers the tail of whatever just played — suspending mid-sound
   * would clip it — and `play()` resumes the context on its own.
   */
  idle() {
    if (!this.ctx || this.ctx.state !== "running" || this.idleTimer != null) return;
    this.idleTimer = window.setTimeout(() => {
      this.idleTimer = null;
      void this.ctx?.suspend();
    }, 1500);
  }

  setVolume(v: number) {
    this.volume = Math.max(0, Math.min(0.2, v));
    if (this.master) this.master.gain.value = this.volume * MASTER_BOOST;
  }

  setEnabled(on: boolean) {
    this.enabled = on;
  }

  /** Nothing plays until then (performance.now() ms): the app starting, a game in front. */
  private hushedUntil = 0;
  hush(ms: number) {
    this.hushedUntil = Math.max(this.hushedUntil, performance.now() + ms);
  }

  /** `pan`: -1 left … 1 right, so a sound about the computer on your left comes from there. */
  play(name: SoundName | string, pan = 0) {
    if (!this.enabled || performance.now() < this.hushedUntil) return;
    if (this.native) {
      invoke("sound_play", { name, volume: Math.min(1, this.volume * MASTER_BOOST), pan }).catch(() => {});
      return;
    }
    const ctx = this.ctx;
    const master = this.master;
    const buf = this.buffers.get(name);
    if (!ctx || !master || !buf) return;
    if (this.idleTimer != null) {
      window.clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
    if (ctx.state === "suspended") void ctx.resume();
    const src = ctx.createBufferSource();
    src.buffer = buf;
    if (pan && ctx.createStereoPanner) {
      const p = ctx.createStereoPanner();
      p.pan.value = Math.max(-1, Math.min(1, pan));
      src.connect(p).connect(master);
    } else {
      src.connect(master);
    }
    src.start();
  }
}

export const Sound = new SoundEngine();
