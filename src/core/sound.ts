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
  error: { notes: [n(0, 0.14, 233, 0.3, "square", 185), n(0.17, 0.2, 196, 0.3, "square", 150)] },
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

  play(name: SoundName | string) {
    if (!this.enabled) return;
    if (this.native) {
      invoke("sound_play", { name, volume: Math.min(1, this.volume * MASTER_BOOST) }).catch(() => {});
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
    src.connect(master);
    src.start();
  }
}

export const Sound = new SoundEngine();
