// The island: DOM shell, sizing animation, Bob placement, mouse handling and
// drag-and-drop. Adapted from Coucou's island controller, with Tandem's status,
// actions and "drop a file on Bob to send it" in place of the Claude views.

import { call, fire, IS_TAURI, onDragDrop, type DragDropPayload, type Dropped } from "../bridge";
import { Spring, Tracked } from "../core/anim";
import {
  EXPANDED_CORNER, EXPANDED_W, NOTCH_W, PANEL_W, ROUNDED_CORNER,
  botGlowColor, botGlowOpacity, botPosition, islandSize,
  type IslandMode, type IslandViewName,
} from "../core/layout";
import { Sound } from "../core/sound";
import { Bob } from "../bob/bob";
import { Greeting } from "../bob/hello";
import type { Snapshot } from "../types";
import { h } from "./dom";
import { IslandStateMachine } from "./fsm";
import { State } from "./state";
import { buildGame } from "./game";
import { buildSuggest, buildTour, type Suggestion, type TourStep } from "./views";
import { agentKey, agents, buildAgent, buildAi, loadPrompts, type AgentEv, type Prompt } from "./views";
import { appName, buildLyrics, buildStats, dayKey, hoursText, listenWeek, loadListen } from "./views";
import { buildDevices, buildDrop, buildFocus, buildHeader, buildHome, buildMedia, buildMoment, buildCharge, buildNotes, buildNowPlaying, buildRecipients, pictureColor, buildNote, buildUpdate, homeText, mediaPosition, parseLrc, spreadLyrics, type UpdateInfo, type ViewActions, type ViewHost } from "./views";
import { saveFocus, type NowPlaying } from "./state";
import { addNote, loadNotes, nextReminder, updateNote } from "../core/notes";
import { nextTip } from "./tips";
import { MomentWatcher, panFor, type Moment } from "./moments";
import { pending } from "../whatsnew";
import { AmbientLayer } from "./ambient";
import { isMidnightOfNewYear, lookFor, skyOf, timeOfDay, timeZone, withWeather, type Look, type Sky } from "../core/seasons";

const BOT_OVERHANG = 40;
/** Same margin as the Rust hit test (src-tauri/src/island.rs). */
const HIT_MARGIN = 14;

const modeOrder = (m: IslandMode) => (m === "hidden" ? 0 : m === "compact" ? 1 : 2);

/** How long a computer may be gone before it counts as gone. */
const OFFLINE_GRACE_MS = 20_000;

/** One picture of what is behind the bar: where it goes (window px) and its pixels. */
export interface BackdropFrame {
  x: number;
  y: number;
  w: number;
  h: number;
  pw: number;
  ph: number;
  rgba: Uint8ClampedArray<ArrayBuffer>;
}

export class Island {
  readonly fsm = new IslandStateMachine();

  private root: HTMLElement;
  private islandEl!: HTMLElement;
  private contentEl!: HTMLElement;
  private viewsEl!: HTMLElement;
  private botCanvas!: HTMLCanvasElement;
  private botGlow!: HTMLElement;
  private greetingCanvas!: HTMLCanvasElement;
  private compactLabel!: HTMLElement;
  private lineEl!: HTMLElement;

  private header!: ViewHost;
  private views = new Map<IslandViewName, ViewHost>();

  private width = new Tracked(NOTCH_W);
  private height = new Tracked(0);
  private radius = new Tracked(ROUNDED_CORNER);
  private botCx = new Spring(46);
  private botCy = new Spring(16);
  private botSize = new Spring(10);

  private engine = new Bob();
  private greeting = new Greeting();

  // The device moments, one at a time.
  private watcher = new MomentWatcher();
  private momentView!: ReturnType<typeof buildMoment>;
  private gameView!: ReturnType<typeof buildGame>;
  private suggestView!: ReturnType<typeof buildSuggest>;
  private tourView!: ReturnType<typeof buildTour>;
  /** Where Bob runs to in the game (island coordinates), or null for his usual place. */
  private gameBobX: number | null = null;
  private chargeView!: ReturnType<typeof buildCharge>;
  private momentQueue: Moment[] = [];
  private momentTimer: number | null = null;
  // The season drifting behind the bar, and what Bob wears.
  private ambient = new AmbientLayer();
  private ambientCanvas!: HTMLCanvasElement;
  private ambientPx = { w: 0, h: 0 };
  private look: Look | null = null;
  // Petting: the pointer stroking back and forth over him.
  private petDir = 0;
  private petFlips: number[] = [];
  private lastPetX = 0;
  private lastPet = 0;

  private running = false;
  private lastFrame = 0;
  private dirty = true;
  private canvasPx = 0;

  /** Mirrors the Rust side: the window starts as the tiny wake strip, and has to be
   *  enlarged (set_collapsed false) before anything can be drawn. */
  private collapsed = true;
  private collapseTimer: number | null = null;
  private wasInIsland = false;
  private pushedRect = { x: -1, y: -1, w: -1, h: -1 };

  private botHovering = false;
  private botHoverTimer: number | null = null;
  private lastLoveTime = 0;
  private botHoverStart = { x: 0, y: 0 };

  /** Transfer bar opened with Ctrl+Alt+U: stays until it is toggled off again. */
  private pillPinned = false;
  /** Transfer bar closed by hand: stays closed until the next transfer starts. */
  private pillSuppressed = false;
  /** Space was pressed while dragging a file: the drop view is up for this drag. */
  private hintActive = false;
  private dropCloseTimer: number | null = null;
  private lastDropAt = 0;
  /** Shift-drop: files waiting while the person picks how to prepare them. */
  private pendingDrop: Dropped | null = null;
  private pendingTarget?: string;

  private confusedRecovery: number | null = null;
  private flashTimer: number | null = null;
  private noteTimer: number | null = null;
  private lastRows = 0;

  constructor(root: HTMLElement) {
    this.root = root;
    this.build();
    this.wireFsm();
    this.wireInput();
    this.engine.onDizzy = () => this.handleDizzy();
    this.refreshLook();
    // The weather outside: now, and every 20 minutes.
    this.ambient.onFlash = () => {
      if (State.mode === "expanded") this.engine.triggerEmote("surprised");
    };
    if (`${State.settings.weather}|${State.settings.city}` !== this.weatherFor) void this.fetchWeather();
    window.setInterval(() => void this.fetchWeather(), 20 * 60_000);
    // A focus that was running when CubeCursor closed carries on.
    this.focusTick();
    this.reminderLoop();
    // Bob's tip of the day, when nothing else is going on.
    window.setInterval(() => this.maybeTip(), 60_000);
    // The look changes with the day; the year turning gets confetti on the minute.
    window.setInterval(() => {
      this.refreshLook();
      if (isMidnightOfNewYear(new Date()) && State.settings.seasons) {
        this.ambient.celebrate();
        this.engine.triggerEmote("proud");
        this.showNote("Happy New Year! 🎉", 4200);
      }
    }, 60_000);
    this.greeting.onComplete = () => this.fsm.greetComplete();
    State.subscribe(() => {
      this.applyTint();
      this.dirty = true;
      this.ensureRunning();
    });
  }

  // ── DOM ─────────────────────────────────────────────────────────────────────

  private build() {
    const actions: ViewActions = {
      collapse: () => this.collapse(),
      blip: () => Sound.play("blip"),
      toggleDevices: () => this.setView(State.view === "devices" ? "home" : "devices"),
      toggleFocus: () => this.setView(State.view === "focus" ? "home" : "focus"),
      toggleMedia: () => this.setView(State.view === "media" ? "home" : "media"),
      toggleNotes: () => this.setView(State.view === "notes" ? "home" : "notes"),
      toggleAi: () => {
        if (State.view === "ai") return this.setView("home");
        this.setView("ai");
        this.aiView.refresh();
      },
      toggleGame: () => {
        if (State.view === "game") return this.setView("home");
        this.setView("game");
        this.gameView.start();
      },
      toggleSound: () => {
        State.settings.soundEnabled = !State.settings.soundEnabled;
        Sound.setEnabled(State.settings.soundEnabled);
        State.saveSettings();
        State.notify();
      },
      dropAction: (act) => {
        State.dropMenu = false;
        const d = this.pendingDrop;
        this.pendingDrop = null;
        State.notify();
        if (d) this.swallow(d, this.pendingTarget, act);
      },
      cancelDrop: () => {
        State.dropMenu = false;
        this.pendingDrop = null;
        State.notify();
        this.engine.animateMorph(0);
        this.closeBar();
      },
    };

    this.lineEl = h("div", { id: "line" });
    this.botGlow = h("div", { id: "bot-glow" });
    this.botCanvas = h("canvas", { id: "bot-canvas" });
    this.greetingCanvas = h("canvas", { id: "greeting-canvas" });
    this.compactLabel = h("div", { id: "compact-label" });

    this.header = buildHeader(actions);
    this.views.set("home", buildHome(actions));
    this.views.set("drop", buildDrop(actions));
    this.views.set("note", buildNote());
    this.momentView = buildMoment({
      takeApps: (id) => { Sound.play("send"); fire("take_apps", { peerId: id }); this.closeBar(); },
      jump: (id) => { fire("engage", { peerId: id }); this.closeBar(); },
    });
    this.views.set("moment", this.momentView);
    this.chargeView = buildCharge();
    this.views.set("charge", this.chargeView);
    this.views.set("devices", buildDevices({
      blip: () => Sound.play("blip"),
      jump: (id) => { fire("engage", { peerId: id }); this.closeBar(); },
      power: (id, action) => fire("power_peer", { peerId: id, action }),
      hear: (id, on) => fire("hear_audio", { peerId: id, on }),
      wake: (id) => fire("wake_peer", { peerId: id }),
    }));
    this.views.set("focus", buildFocus({
      blip: () => Sound.play("blip"),
      start: (min) => this.focusStart(min),
      stop: () => this.focusStop(),
      more: (min) => {
        if (State.focus) {
          State.focus = { endsAt: State.focus.endsAt + min * 60_000, total: State.focus.total + min * 60_000 };
          saveFocus(State.focus);
          State.notify();
        }
      },
    }));
    const mediaSeek = (ms: number) => {
      const m = State.media;
      if (!m) return;
      const to = Math.max(0, Math.min(ms, m.duration_ms || ms));
      // Show the jump at once; Windows confirms it within a moment.
      State.media = { ...m, position_ms: to };
      State.mediaAt = performance.now();
      State.notify();
      void call("media_control", { action: `seek:${Math.round(to)}` }).catch((e) => this.showNote(String(e)));
    };
    const mediaControl = (action: "play_pause" | "next" | "prev") => {
      // Show the change at once; Windows confirms it within a second.
      if (action === "play_pause" && State.media) {
        State.media = { ...State.media, position_ms: mediaPosition(), playing: !State.media.playing };
        State.mediaAt = performance.now();
        State.notify();
      }
      void call("media_control", { action }).catch((e) => this.showNote(String(e)));
    };
    this.views.set("media", buildMedia({
      blip: () => Sound.play("blip"),
      control: (action) => mediaControl(action),
      seek: mediaSeek,
      toggleLyrics: () => this.toggleLyrics(),
      audio: async () => {
        const app = State.media?.app ?? "";
        const [volume, outputs] = await Promise.all([
          call<number | null>("media_volume", { app }).catch(() => null),
          call<{ id: string; name: string; default: boolean }[]>("media_outputs").catch(() => []),
        ]);
        return { volume, outputs };
      },
      setVolume: (v) => void call("media_set_volume", { app: State.media?.app ?? "", volume: v }).catch((e) => this.showNote(String(e), 2600, "home")),
      setOutput: (id) => {
        const was = State.snap?.audio_out;
        if (id.startsWith("pc:")) {
          fire("media_play_on", { peerId: id.slice(3), on: true });
          this.showNote("🔊 Your sound is moving over…", 2400, "home");
          return;
        }
        if (was) fire("media_play_on", { peerId: was, on: false });
        void call("media_set_output", { id }).catch((e) => this.showNote(String(e), 3000, "home"));
      },
      speed: () => this.mediaRate,
      setSpeed: (r) => void call("media_control", { action: `rate:${r}` }).then(
        () => { this.mediaRate = r; State.notify(); },
        (e) => this.showNote(String(e), 3000, "home")),
      sleepLeft: () => (this.sleepAt ? Math.max(1, Math.ceil((this.sleepAt - Date.now()) / 60_000)) : 0),
      cycleSleep: () => {
        const steps = [0, 15, 30, 60];
        const left = this.sleepAt ? Math.ceil((this.sleepAt - Date.now()) / 60_000) : 0;
        const next = steps.find((s) => s > left) ?? 0;
        this.sleepAt = next ? Date.now() + next * 60_000 : 0;
        if (next) this.engine.triggerEmote("yawn", 1);
        State.notify();
      },
      openLyrics: () => {
        if (!State.settings.lyrics) this.toggleLyrics();
        this.setView("lyrics");
      },
      openStats: () => this.setView("stats"),
      spotify: () => call("spotify_now"),
      like: (id, on) => call<void>("spotify_like", { id, on }).then(() => {
        if (on) { this.engine.triggerEmote("love", 1); Sound.play("blip"); }
      }, (e) => { this.showNote(String(e), 3000, "home"); throw e; }),
      playlists: () => call("spotify_playlists"),
      connectSpotify: () => void call("spotify_connect", { clientId: null }).then(
        () => this.showNote("Spotify opened in your browser — press Agree", 3500, "home"),
        (e) => this.showNote(String(e), 4000, "home")),
      addTo: (playlist, id, name) => void call("spotify_add", { playlist, id }).then(
        () => { this.engine.triggerEmote("happy", 0.8); this.showNote(`➕ Added to ${name}`, 2200, "home"); },
        (e) => this.showNote(String(e), 3000, "home")),
    }));
    this.views.set("lyrics", buildLyrics({ back: () => this.setView("media") }));
    this.views.set("stats", buildStats({ back: () => this.setView("media") }));
    window.setInterval(() => this.listenTick(), 5000);
    this.views.set("recipients", buildRecipients({
      blip: () => Sound.play("blip"),
      toggle: (id) => {
        if (State.picked.has(id)) State.picked.delete(id);
        else State.picked.add(id);
        State.notify();
      },
      all: () => {
        const ids = State.connectedPeers.map((p) => p.id);
        State.picked = ids.every((id) => State.picked.has(id)) ? new Set() : new Set(ids);
        State.notify();
      },
      send: () => {
        const ids = [...State.picked].filter((id) => State.connectedPeers.some((p) => p.id === id));
        const d = this.pendingDrop;
        if (!d || !ids.length) return;
        this.pendingDrop = null;
        this.setView("drop");
        this.swallow(d, ids.length === 1 ? ids[0] : ids.join(","));
      },
      cancel: () => {
        this.pendingDrop = null;
        this.closeBar();
      },
    }));
    this.views.set("nowplaying", buildNowPlaying({
      blip: () => Sound.play("blip"),
      control: mediaControl,
      seek: mediaSeek,
      toggleLyrics: () => this.toggleLyrics(),
      open: () => {
        State.isPinned = true;
        this.setView("media");
      },
      close: () => {
        // Hidden until the next song.
        this.npHiddenFor = this.songKey();
        this.closeBar();
      },
    }));
    this.notesView = buildNotes({
      blip: () => Sound.play("blip"),
      typing: (on) => {
        // Writing keeps the bar open; it goes back to normal when the note is kept or dropped.
        if (on) State.isPinned = true;
        fire("island_typing", { on });
      },
      add: (text, at) => {
        State.notes = addNote(text, at);
        Sound.play("approve");
      },
      done: (id) => {
        State.notes = updateNote(id, { done: true, remindAt: null });
        if (State.dueNote === id) State.dueNote = null;
        Sound.play("approve");
        State.notify();
      },
      snooze: (id, min) => {
        State.notes = updateNote(id, { remindAt: Date.now() + min * 60_000 });
        if (State.dueNote === id) State.dueNote = null;
        this.closeBar();
      },
    });
    this.views.set("notes", this.notesView);
    this.updateView = buildUpdate({
      blip: () => Sound.play("blip"),
      install: () => {
        this.updateView.busy("Downloading…");
        call("update_install").catch((e) => {
          this.updateView.busy(null);
          this.showNote(String(e), 4200);
        });
      },
      later: () => this.closeBar(),
      tour: () => {
        // The settings window shows the tour of what's new when it opens.
        fire("open_main_window");
        this.closeBar();
      },
    });
    this.views.set("update", this.updateView);
    this.gameView = buildGame({
      bob: () => {
        const o = this.gameOrigin();
        return { x: this.botCx.value - o.x, y: this.botCy.value - o.y };
      },
      moveBob: (x) => {
        this.gameBobX = x == null ? null : x + this.gameOrigin().x;
        this.ensureRunning();
      },
      caught: (streak, record) => {
        Sound.play("pop");
        this.engine.triggerEmote(record && streak > 2 ? "proud" : streak % 5 === 0 ? "love" : "happy", 0.9);
        if (record && streak > 2) this.ambient.celebrate();
        this.moodUp(0.3);
      },
      missed: () => {
        Sound.play("blip");
        this.engine.triggerEmote("annoyed", 0.9);
      },
    });
    this.views.set("game", this.gameView);
    this.suggestView = buildSuggest(() => this.closeBar());
    this.views.set("suggest", this.suggestView);
    this.agentView = buildAgent({ answer: (e, allow) => this.agentAnswer(e, allow), close: () => this.closeBar() });
    this.views.set("agent", this.agentView);
    this.aiView = buildAi({
      answer: (e, allow) => this.agentAnswer(e, allow),
      ask: (q) => call<string>("ai_ask", { question: q }),
      copy: (text, what) => void call("ai_copy", { text }).then(() => this.showNote(`${what} is copied — paste it with Ctrl+V`, 2600, "home"), (e) => this.showNote(String(e))),
      usage: () => call("ai_usage").catch(() => null) as never,
    });
    this.views.set("ai", this.aiView);
    this.tourView = buildTour({ next: () => this.tourStep(this.tourAt + 1), skip: () => this.endTour() });
    this.views.set("tour", this.tourView);
    this.viewsEl = h("div", { id: "views" });
    for (const v of this.views.values()) this.viewsEl.append(v.el);
    this.contentEl = h("div", { id: "content" }, this.header.el, this.viewsEl);

    this.ambientCanvas = h("canvas", { id: "ambient-canvas" });
    // Liquid Glass: what is behind the bar, blurred, and again bent along its edges.
    this.bdEl = h("canvas", { id: "bd" }) as HTMLCanvasElement;
    this.bdEdgeImg = h("canvas", { id: "bd-edge-img" }) as HTMLCanvasElement;
    const glass = h("div", { id: "glass" }, this.bdEl, h("div", { id: "bd-edge" }, this.bdEdgeImg));
    const clip = h("div", { id: "island-clip" }, glass, h("div", { id: "bar-tint" }), this.ambientCanvas, this.greetingCanvas, this.contentEl);
    this.islandEl = h("div", { id: "island" }, clip, this.botGlow, this.botCanvas, this.compactLabel);

    const dpr = Math.min(2, window.devicePixelRatio || 1);
    this.greetingCanvas.width = Math.round(EXPANDED_W * dpr);
    this.greetingCanvas.height = Math.round(150 * dpr);
    this.greetingCanvas.style.width = `${EXPANDED_W}px`;
    this.greetingCanvas.style.height = "150px";

    this.root.append(this.lineEl, this.islandEl);
    this.applyGeometry();
  }

  // ── FSM ─────────────────────────────────────────────────────────────────────

  private wireFsm() {
    this.fsm.homeToPetitDelay = State.settings.autoCloseInterval;
    this.fsm.idleTarget = () => (this.wantPill() ? "petit" : "hidden");
    this.fsm.petitHold = () => this.wantPill();
    this.fsm.onTransition = (from, to) => {
      switch (to) {
        case "hidden":
          if (from === "hello") {
            this.greeting.interrupt();
            State.view = State.defaultView();
          }
          this.setMode("hidden");
          // A full-screen app gets its screen back right away.
          fire("end_reveal");
          break;
        case "petit":
          if (from === "hello") this.greeting.interrupt();
          else if (from === "hidden" && !this.auto) Sound.play("peek");
          this.setMode("compact");
          if (from === "hello") State.view = State.defaultView();
          if (!this.wasInIsland) this.fsm.mouseLeft();
          break;
        case "home":
          this.expand(State.defaultView());
          if (!this.wasInIsland) this.fsm.mouseLeft();
          break;
        case "hello":
          this.expand("greeting");
          this.greeting.start();
          break;
      }
      State.notify();
    };
  }

  /** Should the small transfer bar be up right now? */
  private wantPill(): boolean {
    if (this.quiet || this.pillSuppressed) return false;
    return this.pillPinned || State.activeTransfers.length > 0 || State.staging > 0;
  }

  // ── Bob's mood ──────────────────────────────────────────────────────────────

  private mood = { v: 60, last: Date.now() };
  private loadMood() {
    try { this.mood = { ...this.mood, ...JSON.parse(localStorage.getItem("cc.mood") ?? "{}") }; } catch { /* fine */ }
    const days = (Date.now() - this.mood.last) / 864e5;
    // Left alone for days, he gets a little sad (a day off is fine).
    if (days > 1) this.mood.v = Math.max(5, this.mood.v - 12 * (days - 1));
    this.engine.mood = this.mood.v / 100;
    if (days >= 3) {
      window.setTimeout(() => {
        this.engine.triggerEmote("yawn", 1.6);
        window.setTimeout(() => {
          this.autoly(() => this.showNote("I missed you! 🥺 Where have you been?", 4200));
          this.engine.triggerEmote("love");
          this.moodUp(8);
        }, 1800);
      }, 7000);
    }
    this.moodUp(0);
    // Every 10 minutes you're here, he likes you a bit more.
    window.setInterval(() => this.moodUp(0.5), 10 * 60_000);
  }
  moodUp(by: number) {
    this.mood = { v: Math.max(0, Math.min(100, this.mood.v + by)), last: Date.now() };
    this.engine.mood = this.mood.v / 100;
    try { localStorage.setItem("cc.mood", JSON.stringify(this.mood)); } catch { /* fine */ }
  }

  // ── The tour ────────────────────────────────────────────────────────────────

  private tourAt = 0;
  private static TOUR: (TourStep & { emote: "happy" | "wink" | "love" | "proud" | "surprised" })[] = [
    { title: "Hi, I'm Bob! 👋", sub: "I live up here at the top of your screen. Let me show you around — it takes half a minute.", emote: "happy" },
    { title: "One mouse, two computers", sub: "Push your cursor through the edge of the screen and it jumps to your other computer — keyboard and clipboard too.", emote: "wink" },
    { title: "Send files by dropping them on me", sub: "Pick up a file, press Space, and drop it on me. It flies over to the other computer.", emote: "surprised" },
    { title: "Everything is in this bar", sub: "Notes, your music with lyrics, a focus timer, your computers and a little game — the icons up top.", emote: "proud" },
    { title: "Make it yours", sub: "Liquid Glass or Classic, the bar's colour, my outfit and my colour: it's all in settings. Have fun!", emote: "love" },
  ];
  startTour() {
    this.tourStep(0);
  }
  private tourStep(i: number) {
    const steps = Island.TOUR;
    if (i >= steps.length) return this.endTour();
    this.tourAt = i;
    this.tourView.show(steps[i], i, steps.length);
    this.engine.triggerEmote(steps[i].emote, 1.4);
    if (State.view !== "tour" || State.mode !== "expanded") this.autoly(() => this.alert("tour"));
    Sound.play("blip");
  }
  private endTour() {
    try { localStorage.setItem("cc.tourDone", "1"); } catch { /* fine */ }
    if (State.view === "tour") this.closeBar();
  }

  // ── Music: speed, sleep timer, what you listened to ──────────────────────────

  private mediaRate = 1;
  private sleepAt = 0;
  private lastSong = "";

  /** Every 5 s: the sleep timer, and adding up what plays. */
  private listenTick() {
    if (this.sleepAt && Date.now() >= this.sleepAt) {
      this.sleepAt = 0;
      void call("media_control", { action: "pause" }).catch(() => {});
      this.engine.triggerEmote("yawn", 4);
      this.autoly(() => this.showNote("Good night 🌙 The music is off.", 4000));
      State.notify();
    }
    const m = State.media;
    if (m?.title !== this.lastSong) {
      this.lastSong = m?.title ?? "";
      this.mediaRate = 1;
    }
    if (m?.playing && !State.video) {
      const days = loadListen();
      const k = dayKey();
      const d = days[k] ?? { t: 0, a: {}, s: {} };
      d.t += 5;
      const artist = (m.artist || appName(m.app)).split(/,|&| feat\./i)[0].trim();
      if (artist) d.a[artist] = (d.a[artist] ?? 0) + 5;
      const song = m.artist ? `${m.title} — ${artist}` : m.title;
      if (song) d.s[song] = (d.s[song] ?? 0) + 5;
      days[k] = d;
      // Two weeks is plenty.
      for (const key of Object.keys(days)) if (Date.now() - new Date(key).getTime() > 15 * 864e5) delete days[key];
      try { localStorage.setItem("cc.listen", JSON.stringify(days)); } catch { /* fine */ }
    }
    // Sunday evening: the week in music.
    const now = new Date();
    if (now.getDay() === 0 && now.getHours() >= 18) {
      const w = listenWeek();
      if (w.secs > 3600) this.suggest("listen-week", {
        title: `${hoursText(w.secs)} of music this week 🎧`,
        sub: w.artists[0] ? `You played ${w.artists[0][0]} the most.` : "Nice taste.",
        act: ["See more", () => window.setTimeout(() => this.setView("stats"), 50)],
      });
    }
  }

  // ── AI agents ───────────────────────────────────────────────────────────────

  private agentView!: ReturnType<typeof buildAgent>;
  private aiView!: ReturnType<typeof buildAi>;

  /** Something from an AI agent on any computer. */
  onAgent(e: AgentEv) {
    const k = agentKey(e);
    const shown = this.agentView.current();
    if (e.state === "ended") agents.delete(k);
    else if (e.state === "allowed" || e.state === "denied" || e.state === "terminal") {
      agents.set(k, { ...e, state: e.state === "terminal" ? "waiting" : "working", text: "" });
      if (shown?.id === e.id && State.view === "agent") this.closeBar();
    } else agents.set(k, e);
    if (State.view === "ai") this.aiView.refresh();
    if (!State.settings.aiAgents || !["approve", "waiting", "done"].includes(e.state)) return;
    this.agentView.show(e);
    this.autoly(() => this.alert("agent"));
    if (e.state === "approve") {
      this.engine.triggerEmote("surprised", 1);
      Sound.play("blip");
    } else if (e.state === "done") {
      this.engine.gesture("thumbs");
      window.setTimeout(() => { if (State.view === "agent" && this.agentView.current()?.id === e.id && !this.wasInIsland) this.closeBar(); }, 9000);
    } else {
      this.engine.triggerEmote("wink");
      window.setTimeout(() => { if (State.view === "agent" && this.agentView.current()?.id === e.id && !this.wasInIsland) this.closeBar(); }, 9000);
    }
  }

  private agentAnswer(e: AgentEv, allow: boolean) {
    fire("agent_answer", { id: e.id, pc: e.pc, allow });
    this.engine.triggerEmote(allow ? "love" : "annoyed", 0.6);
    if (State.view === "agent") this.closeBar();
  }

  /** You copied what looks like an error: Bob offers to look at it. */
  onClipError(text: string) {
    if (!State.settings.aiErrors || State.mode === "expanded" || State.focus) return;
    const first = text.split("\n").find((l) => /error|exception|panic|failed|traceback/i.test(l)) ?? text.split("\n")[0];
    this.suggestView.show({ title: "That looks like an error 🐞", sub: first.trim().slice(0, 110), act: ["Ask Bob", () => window.setTimeout(() => {
      this.setView("ai");
      this.aiView.refresh();
      this.aiView.askNow(`I got this error. What's wrong and how do I fix it?\n\n${text}`);
    }, 50)] });
    this.autoly(() => this.alert("suggest"));
    this.engine.triggerEmote("surprised", 0.6);
    window.setTimeout(() => { if (State.view === "suggest" && !this.wasInIsland) this.closeBar(); }, 9000);
  }

  /** Something arrived from the other computer for an AI here. */
  onAiIn(m: { from: string; preview: string; image: boolean }) {
    this.engine.triggerEmote("surprised", 0.5);
    this.autoly(() => this.showNote(`📋 ${m.image ? m.preview : `“${m.preview}”`} from ${m.from} — paste it into your AI with Ctrl+V`, 5000));
  }
  onAiSent(m: { to: string; shot: boolean }) {
    this.autoly(() => this.showNote(`✈️ ${m.shot ? "Screenshot" : "Sent"} to ${m.to} — paste it there with Ctrl+V`, 3200));
  }

  /** The prompt library changed here (the settings window): to the other computers. */
  promptsChanged() {
    fire("ai_relay", { kind: "ui:prompts", json: JSON.stringify(loadPrompts()) });
  }
  /** …or there: newest version of each wins. A video sent over plays on here. */
  onRelay(m: { kind: string; json: string; fromName?: string }) {
    if (m.kind === "ui:video") {
      try {
        const v = JSON.parse(m.json) as { id: string; title: string; ms: number };
        State.video = { id: v.id, title: v.title, startMs: v.ms, nowMs: v.ms };
        this.engine.triggerEmote("surprised", 0.6);
        this.openVideoWindow();
        State.notify();
      } catch { /* not for us */ }
      return;
    }
    if (m.kind !== "ui:prompts") return;
    let theirs: Prompt[] = [];
    try { theirs = JSON.parse(m.json) as Prompt[]; } catch { return; }
    const mine = new Map(loadPrompts().map((p) => [p.id, p]));
    let changed = false;
    for (const p of theirs) {
      const have = mine.get(p.id);
      if (!have || p.at > have.at) { mine.set(p.id, p); changed = true; }
    }
    if (changed) try { localStorage.setItem("cc.prompts", JSON.stringify([...mine.values()])); } catch { /* fine */ }
    if (State.view === "ai") this.aiView.refresh();
  }

  // ── Suggestions ─────────────────────────────────────────────────────────────

  /** Bob suggests something (once per key per day), only when the bar is free. */
  private suggest(key: string, s: Suggestion) {
    if (this.quiet || State.mode === "expanded" || State.focus) return;
    const today = new Date().toDateString();
    const k = `cc.sg.${key}`;
    try {
      if (localStorage.getItem(k) === today) return;
      localStorage.setItem(k, today);
    } catch { /* fine */ }
    this.suggestView.show(s);
    this.autoly(() => this.alert("suggest"));
    this.engine.triggerEmote("wink");
    window.setTimeout(() => {
      if (State.view === "suggest" && !this.wasInIsland) this.closeBar();
    }, 12_000);
  }

  /** Once a minute: is there something worth suggesting? */
  private think() {
    const now = new Date(), hr = now.getHours();
    // You usually start a focus around this hour, and haven't today.
    let starts: string[] = [];
    try { starts = JSON.parse(localStorage.getItem("cc.focusStarts") ?? "[]"); } catch { /* fine */ }
    const days = new Set(starts.filter((s) => new Date(s).getHours() === hr && new Date(s).toDateString() !== now.toDateString()).map((s) => new Date(s).toDateString()));
    const today = starts.some((s) => new Date(s).toDateString() === now.toDateString());
    if (days.size >= 3 && !today && now.getMinutes() < 30) {
      const mins = State.settings.focusMinutes || 25;
      this.suggest("focus", { title: "Time to focus?", sub: `You usually start one around ${hr}:00. ${mins} minutes, and I'll keep time.`, act: [`Start ${mins} min`, () => this.focusStart(mins)] });
      return;
    }
    // A wet morning.
    const w = this.weather;
    if (w && hr >= 7 && hr < 11 && (w.sky === "rain" || w.sky === "storm" || w.sky === "drizzle")) {
      this.suggest("umbrella", { title: `It's raining in ${w.city} ☔`, sub: "Take an umbrella if you're heading out today." });
      return;
    }
    // A cold one.
    if (w && hr >= 7 && hr < 11 && w.temp <= 3) {
      this.suggest("cold", { title: `${w.temp}° in ${w.city} 🧣`, sub: "Wrap up warm if you're going out." });
      return;
    }
    // Late at night.
    if (hr >= 23 && !State.settings.calm) {
      this.suggest("late", { title: "It's getting late 🌙", sub: "Want me to be quiet for the night? Calm Bob turns off the drifting and the sounds.", act: ["Calm Bob", () => { State.settings.calm = true; State.saveSettings(); this.applySettings(); }] });
    }
  }

  // ── "Hey Bob" ───────────────────────────────────────────────────────────────

  private voiceOn = false;
  private applyVoice() {
    const on = !!State.settings.voice;
    if (on === this.voiceOn) return;
    this.voiceOn = on;
    void call("voice_set", { on }).then(
      () => { if (on) this.autoly(() => this.showNote("I'm listening! Say \"Hey Bob…\" 👂", 3200)); },
      (e) => {
        this.voiceOn = false;
        State.settings.voice = false;
        State.saveSettings();
        this.autoly(() => this.showNote(String(e), 6000));
      },
    );
  }

  /** A phrase Bob heard. */
  onVoice(text: string) {
    const t = text.toLowerCase().replace(/^hey bob\s*/, "").trim();
    this.engine.triggerEmote("surprised", 0.6);
    this.moodUp(0.2);
    const media = (action: string) => void call("media_control", { action }).catch((e) => this.showNote(String(e)));
    const open = (v: Parameters<Island["setView"]>[0]) => this.autoly(() => this.alert(v));
    const peer = State.snap?.paired.find((p) => p.connected);
    switch (t) {
      case "": this.autoly(() => this.showNote("Yes? 👂", 2000)); break;
      case "play": media("play"); break;
      case "pause": media("pause"); break;
      case "next song": media("next"); break;
      case "previous song": media("prev"); break;
      case "show the lyrics":
        State.settings.lyrics = true;
        State.saveSettings();
        open("media");
        break;
      case "start focus": this.focusStart(State.settings.focusMinutes || 25); open("focus"); break;
      case "open my notes": open("notes"); break;
      case "show my computers": open("devices"); break;
      case "play catch": open("game"); this.gameView.start(); break;
      case "show my agents": open("ai"); this.aiView.refresh(); break;
      case "what's the weather": {
        const w = this.weather;
        if (!w) this.autoly(() => this.showNote("I don't know the weather yet. Is it on in settings?", 3500));
        else {
          this.chargeView.showWeather(w);
          open("charge");
          window.setTimeout(() => { if (State.view === "charge") this.closeBar(); }, 4000);
        }
        break;
      }
      case "jump":
      case "go to the other computer": fire("engage", { peerId: null }); break;
      case "play its sound here":
        if (peer) fire("hear_audio", { peerId: peer.id, on: true });
        else this.autoly(() => this.showNote("No other computer is connected.", 3000));
        break;
      case "stop the sound":
        if (State.snap?.audio_in) fire("hear_audio", { peerId: State.snap.audio_in, on: false });
        break;
      case "close": this.closeBar(); break;
      case "how are you": {
        const m = this.mood.v;
        const say = m > 75 ? "Amazing — I love hanging out with you! 💙" : m > 45 ? "Pretty good! Thanks for asking 😊" : "A bit lonely… but better now you're here 🥺";
        this.engine.triggerEmote(m > 45 ? "love" : "yawn");
        this.autoly(() => this.showNote(say, 4000));
        break;
      }
    }
  }

  launch() {
    this.loadMood();
    this.applyVoice();
    window.setInterval(() => this.think(), 60_000);
    // The tour, the first time CubeCursor runs (after the hello).
    let toured = "";
    try { toured = localStorage.getItem("cc.tourDone") ?? ""; } catch { /* fine */ }
    if (!toured && !this.quiet) window.setTimeout(() => this.startTour(), 6500);
    // Bob says hello once a day, not every time the computer starts. The very first
    // time he does it out loud; after that he waves without a sound.
    const today = new Date().toDateString();
    let seen: string | null = "";
    try { seen = localStorage.getItem("cc.hello"); localStorage.setItem("cc.hello", today); } catch { /* fine */ }
    if (seen !== today && !this.quiet) {
      if (seen != null) Sound.hush(4500);
      this.autoly(() => this.fsm.launch());
    }
    // A good-morning stretch (or a sleepy yawn late at night), once a day, silently.
    const tod = timeOfDay(new Date());
    if (tod === "morning" || tod === "night") {
      let last = "";
      try { last = localStorage.getItem("cc.stretch") ?? ""; } catch { /* fine */ }
      const today = new Date().toDateString();
      if (last !== today) {
        try { localStorage.setItem("cc.stretch", today); } catch { /* fine */ }
        window.setTimeout(() => {
          this.engine.triggerEmote("yawn", 2.2);
        }, 4200);
      }
    }
  }

  // ── Snapshot from Rust ──────────────────────────────────────────────────────

  applySnapshot(s: Snapshot) {
    const prev = State.snap;
    State.snap = s;
    if (prev) {
      // Connection changes don't pop anything up when moments are off: the line at the
      // top changes colour.
      if (!State.settings.moments) {
        if (!prev.connected && s.connected) {
          this.ping("approve");
          this.engine.triggerEmote("happy");
        } else if (prev.connected && !s.connected) {
          this.ping("blip");
        }
      }
      // Crossing to another computer: an optional soft whoosh from that side.
      if (State.settings.crossWhoosh && prev.control !== s.control && (s.control === "remote" || prev.control === "remote")) {
        const peer = s.paired.find((p) => p.id === (s.control_peer ?? prev.control_peer));
        if (!this.quiet) Sound.play("whoosh", panFor(peer?.side ?? null) * (s.control === "remote" ? 1 : -1));
      }
      // Switching computers is silent: the line at the top already changes colour, and a
      // sound on every crossing is just noise.
      for (const t of s.transfers) {
        const before = prev.transfers.find((p) => p.id === t.id && p.direction === t.direction);
        if (!before) {
          // A paper plane flies to (or from) the computer's side.
          const side = s.paired.find((p) => p.name === t.peer || p.id === t.peer)?.side ?? "Right";
          this.paperPlane(t.direction === "out", /left/i.test(side) ? -1 : 1);
          // A new transfer brings the pill back, even if it was closed by hand.
          this.pillSuppressed = false;
          this.reveal();
          // Only the first file of an incoming batch makes a sound.
          if (t.direction === "in" && !prev.transfers.some((p) => p.direction === "in" && p.state === "active")) this.ping("attach");
        } else if (before.state === "active" && t.state === "done") {
          // Ten files sent at once: one chime when the last one lands, not ten.
          if (!s.transfers.some((x) => x.state === "active")) {
            this.flashState("finished", 2200);
            this.ping("finish");
            this.engine.gesture("thumbs");
          }
          this.reveal();
        } else if (before.state !== "error" && t.state === "error") {
          this.pillSuppressed = false;
          this.flashState("error", 3200);
          this.ping("error");
          // A send that failed (not one you cancelled): Bob facepalms.
          if (t.message !== "Cancelled") this.engine.gesture("facepalm");
          this.reveal();
        }
      }
    }
    for (const m of this.watcher.diff(s)) this.enqueueMoment(m);
    const rows = State.transferRows;
    if (rows !== this.lastRows) {
      this.lastRows = rows;
      if (State.mode === "expanded") this.animateGeometry(false);
    }
    State.notify();
  }

  // ── Device moments ─────────────────────────────────────────────────────────

  private enqueueMoment(m: Moment) {
    // This computer's charger: the chime plays the instant it clicks in (or the battery
    // fills), day or night, like a phone's, even when the card itself has to wait or
    // cards are turned off. Only a full-screen game or film keeps it quiet.
    if (m.id === "me" && (m.kind === "plug" || m.kind === "full") && !this.quiet && State.settings.soundEnabled) {
      Sound.play(m.kind);
      m.chimed = true;
    }
    if (m.kind === "acc-on") {
      // Its own switch ("Earbuds' battery"), whatever the other cards do. Accessories
      // already connected when CubeCursor starts aren't news.
      if (!State.settings.accCards || performance.now() - this.launchedAt < 20_000) return;
      this.queueMoment(m);
      return;
    }
    if (!State.settings.moments) return;
    if (m.kind === "offline") {
      // Sleep, a Wi-Fi blip, a restart: wait a little before saying it went away, and
      // say nothing if it is back by then.
      this.awaySince.set(m.id, Date.now());
      window.clearTimeout(this.offlineTimers.get(m.id));
      this.offlineTimers.set(m.id, window.setTimeout(() => {
        this.offlineTimers.delete(m.id);
        if (State.snap?.paired.find((p) => p.id === m.id)?.connected === false) this.queueMoment(m);
      }, OFFLINE_GRACE_MS));
      return;
    }
    if (m.kind === "online") {
      const pending = this.offlineTimers.get(m.id);
      const since = this.awaySince.get(m.id);
      this.awaySince.delete(m.id);
      if (pending != null) {
        window.clearTimeout(pending);
        this.offlineTimers.delete(m.id);
        return;
      }
      // Connecting right after CubeCursor starts is expected, and so is a quick return.
      if (performance.now() - this.launchedAt < 60_000) return;
      if (since != null && Date.now() - since < 2 * 60_000) return;
    }
    this.queueMoment(m);
  }

  private queueMoment(m: Moment) {
    if (!State.settings.moments && m.kind !== "acc-on") return;
    // Only the latest of each kind per computer waits (plugged in, then out: just "out").
    this.momentQueue = this.momentQueue.filter((q) => q.id !== m.id || (q.kind !== m.kind && !(isPlug(q) && isPlug(m))));
    this.momentQueue.push(m);
    this.nextMoment();
  }

  private nextMoment() {
    if (this.momentTimer != null) return;
    if (this.quiet || State.focus) {
      // A game or film is full-screen, or the person is focusing: keep only what
      // matters for later.
      this.momentQueue = this.momentQueue.filter((m) => m.kind === "low");
      if (!(State.focus && !this.quiet && this.momentQueue.length)) return;
    }
    // Never take the bar away from someone using it.
    if (State.mode === "expanded" && State.view !== "moment" && State.view !== "charge" && this.wasInIsland) {
      this.momentTimer = window.setTimeout(() => { this.momentTimer = null; this.nextMoment(); }, 2500);
      return;
    }
    const m = this.momentQueue.shift();
    if (!m) return;
    State.moment = m;
    // Quick news gets the phone's slim pill; a battery running low keeps the card (and its buttons).
    const view = isPlug(m) || m.kind === "online" || m.kind === "offline" || m.kind === "acc-on" ? "charge" : "moment";
    (view === "charge" ? this.chargeView : this.momentView).show(m);
    this.autoly(() => this.alert(view));
    this.react(m);
    const dur = view === "charge" ? 2600 : m.kind === "low" && m.id !== "me" ? 8000 : m.kind === "plug" ? 4800 : 3800;
    const done = () => {
      this.momentTimer = null;
      if (State.view === "moment" || State.view === "charge") {
        // Still being looked at: wait a little longer.
        if (this.wasInIsland) {
          this.momentTimer = window.setTimeout(done, 1500);
          return;
        }
        if (this.momentQueue.length === 0) this.closeBar();
      }
      State.moment = null;
      this.nextMoment();
    };
    this.momentTimer = window.setTimeout(done, dur);
  }

  /** Bob's reaction and the sound, from the side that computer sits on. */
  private react(m: Moment) {
    const pan = panFor(m.side);
    // Late at night (11 pm – 7 am) the cards stay silent, except a battery about to run out.
    const hour = new Date().getHours();
    const calm = State.settings.calm || hour >= 23 || hour < 7;
    const sound = (n: Parameters<typeof Sound.play>[0], always = false) => {
      if (!calm || always) Sound.play(n, pan);
    };
    switch (m.kind) {
      case "plug":
        if (!m.chimed) sound("plug", true);
        this.engine.triggerEmote("happy");
        this.engine.emit("spark", 6);
        break;
      case "unplug":
        sound("unplug");
        this.engine.blink();
        break;
      case "low":
        sound("low", true);
        this.engine.triggerEmote("yawn", 2.4);
        break;
      case "full":
        if (!m.chimed) sound("full");
        this.engine.triggerEmote("proud");
        this.engine.emit("star", 5);
        break;
      case "online":
        sound("hello");
        this.engine.triggerEmote("happy");
        this.engine.hop();
        break;
      case "offline":
        sound("bye");
        this.engine.blink();
        break;
      case "accessory":
        sound("tink");
        this.engine.triggerEmote("surprised");
        break;
      case "acc-on":
        sound("pop");
        this.engine.triggerEmote("happy");
        break;
    }
  }

  // ── Bob's look: the season, a special day ───────────────────────────────────

  private glassOn: boolean | null = null;
  /** Liquid Glass or the classic black bar: the page, Bob and the season all follow. */
  private applyTheme() {
    const glass = State.settings.theme !== "dark";
    document.documentElement.dataset.theme = glass ? "glass" : "dark";
    const bar = State.settings.barColor;
    const carbon = bar === "carbon" || bar === "midnight";
    if (bar) document.documentElement.dataset.bar = bar;
    else delete document.documentElement.dataset.bar;
    // Carbon glass is dark glass: white words, whatever is behind.
    if (carbon) this.tone = "dark";
    // The accent colour the person picked recolours the bar (dots, glow, progress, buttons).
    const accent = State.settings.accent;
    if (accent) document.documentElement.style.setProperty("--teal", accent);
    else document.documentElement.style.removeProperty("--teal");
    this.applyTint();
    if (!glass) {
      delete document.documentElement.dataset.liquid;
      this.bd = null;
    }
    if (this.glassOn !== glass) {
      this.glassOn = glass;
      fire("set_glass", { on: glass });
    }
    document.documentElement.dataset.tone = this.tone;
    this.engine.glass = glass;
    // The hello is drawn, not styled: its words follow what is behind the glass too.
    this.greeting.light = glass && !carbon && !(this.bd && this.tone === "dark");
    this.ambient.light = glass;
    this.dirty = true;
  }

  /** Where the game's view sits inside the island. */
  private gameOrigin() {
    const g = this.gameView.el.getBoundingClientRect(), i = this.islandEl.getBoundingClientRect();
    return { x: g.left - i.left, y: g.top - i.top };
  }

  /** A paper plane: out from Bob to the computer's side, or in from it to Bob. */
  private paperPlane(out: boolean, dir: number) {
    if (this.quiet || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const NS = "http://www.w3.org/2000/svg";
    const mk = <K extends keyof SVGElementTagNameMap>(tag: K, at: Record<string, string>) => {
      const e = document.createElementNS(NS, tag);
      for (const [k, v] of Object.entries(at)) e.setAttribute(k, v);
      return e;
    };
    const w = this.width.value, hh = Math.max(40, this.height.value);
    const bot = this.botCanvas.getBoundingClientRect(), isl = this.islandEl.getBoundingClientRect();
    const bx = bot.width ? bot.left - isl.left + bot.width / 2 : w / 2, by = bot.height ? bot.top - isl.top + bot.height * 0.35 : hh / 2;
    const ex = dir > 0 ? w + 40 : -40, ey = hh * 0.3;
    const [x0, y0, x1, y1] = out ? [bx, by, ex, ey] : [ex, ey, bx, by];
    // A loop-free arc that rises over the bar and comes down at the end.
    const d = `M${x0} ${y0} C${x0 + (x1 - x0) * 0.25} ${Math.min(y0, y1) - 46}, ${x0 + (x1 - x0) * 0.7} ${Math.min(y0, y1) - 30}, ${x1} ${y1}`;
    // The dotted trail it leaves.
    const layer = mk("svg", { class: "plane-trail", width: String(w), height: String(hh), viewBox: `0 0 ${w} ${hh}` });
    const trail = mk("path", { d, pathLength: "100", "stroke-dasharray": "1.4 2.6" });
    layer.append(trail);
    // Folded paper: the wing in light, the fold in shade.
    const plane = mk("svg", { class: "plane", width: "26", height: "26", viewBox: "0 0 24 24" });
    plane.append(
      mk("path", { d: "M1.5 11.2L22.5 3 11.6 13.2z", fill: "#ffffff" }),
      mk("path", { d: "M11.6 13.2L22.5 3 14.6 21z", fill: "#e3e9f2" }),
      mk("path", { d: "M11.6 13.2L14.6 21 9.6 17.4z", fill: "#b9c5d4" }),
      mk("path", { d: "M11.6 13.2L22.5 3", stroke: "#9fb0c4", "stroke-width": "0.5", fill: "none" }),
    );
    plane.style.offsetPath = `path("${d}")`;
    this.islandEl.append(layer, plane);
    const ease = "cubic-bezier(0.5, 0, 0.35, 1)";
    const a = plane.animate(
      [
        { offsetDistance: "0%", scale: out ? "0.3" : "1", opacity: out ? 0.4 : 0 },
        { offsetDistance: "12%", scale: "1", opacity: 1, offset: 0.15 },
        { offsetDistance: "88%", scale: "1", opacity: 1, offset: 0.85 },
        { offsetDistance: "100%", scale: out ? "0.8" : "0.3", opacity: out ? 0 : 0.4 },
      ],
      { duration: 1300, easing: ease, fill: "forwards" },
    );
    trail.animate(
      [{ strokeDashoffset: "100", opacity: 0.55 }, { strokeDashoffset: "0", opacity: 0.55, offset: 0.85 }, { strokeDashoffset: "0", opacity: 0 }],
      { duration: 1700, easing: ease, fill: "forwards" },
    );
    a.onfinish = () => {
      plane.remove();
      window.setTimeout(() => layer.remove(), 450);
      if (!out) this.engine.triggerEmote("happy");
    };
    if (out) this.engine.triggerEmote("wink");
  }

  /** Easter: an egg hidden somewhere in the bar. Found, it hatches confetti. */
  private egg: HTMLElement | null = null;
  private hideEgg() {
    const on = this.look?.special === "easter";
    if (!on) {
      this.egg?.remove();
      this.egg = null;
      return;
    }
    if (this.egg) return;
    const egg = h("button", { class: "egg", title: "An egg!", "aria-label": "Easter egg" });
    egg.style.left = `${30 + Math.random() * 560}px`;
    egg.style.top = `${60 + Math.random() * 70}px`;
    egg.onclick = (e) => {
      e.stopPropagation();
      egg.classList.add("found");
      Sound.play("approve");
      this.ambient.celebrate();
      this.engine.triggerEmote("love");
      this.ensureRunning();
      window.setTimeout(() => {
        egg.remove();
        if (this.egg === egg) this.egg = null;
        // Another one, somewhere else, a bit later.
        window.setTimeout(() => this.hideEgg(), 90_000);
      }, 600);
    };
    this.egg = egg;
    this.contentEl.append(egg);
  }

  /** The weather where you are (null until it is known, or when it's turned off). */
  private weather: { city: string; temp: number; sky: Sky; day: boolean } | null = null;

  private weatherFor = "";

  private async fetchWeather() {
    this.weatherFor = `${State.settings.weather}|${State.settings.city}`;
    if (!State.settings.weather) {
      if (this.weather) {
        this.weather = null;
        this.refreshLook();
      }
      return;
    }
    // The city typed in settings, or the one in the time zone ("Europe/Athens" → "Athens").
    const place = State.settings.city.trim() || timeZone().split("/").pop()?.replace(/_/g, " ") || "";
    if (!place) return;
    try {
      const w = (await call("weather", { place })) as { city: string; temp_c: number; code: number; is_day: boolean; wind_kmh: number };
      const before = this.weather;
      this.weather = { city: w.city, temp: Math.round(w.temp_c), sky: skyOf(w.code), day: w.is_day };
      // Windy from about 20 km/h, a gale at 60.
      const wind = Math.max(0, Math.min(1, (w.wind_kmh - 20) / 40));
      this.ambient.wind = wind;
      this.engine.wind = wind;
      this.engine.snowing = this.weather.sky === "snow";
      const wet = (s?: Sky) => s === "rain" || s === "drizzle" || s === "storm";
      if (before && wet(before.sky) && !wet(this.weather.sky) && this.weather.day) this.ambient.rainbow();
      this.refreshLook();
      if (!before || before.sky !== this.weather.sky) this.weatherNews(!before);
    } catch (e) {
      fire("log_js", { msg: `weather: ${e}` });
    }
  }

  /** The sky changed (or it's the first look of the day): Bob reacts, and a pill says it. */
  private weatherNews(first: boolean, retry = false) {
    const w = this.weather;
    if (!w || this.quiet || State.focus) return;
    const emote = ({ storm: "surprised", rain: "annoyed", drizzle: "annoyed", snow: "love", clear: "happy", clouds: null, fog: "yawn" } as const)[w.sky];
    if (emote && !retry) window.setTimeout(() => this.engine.triggerEmote(emote), 600);
    // Once a day on its own, and whenever the sky turns — never over someone using the bar.
    const key = "cc.weatherShown";
    let seen = "";
    try { seen = localStorage.getItem(key) ?? ""; } catch { /* fine */ }
    const today = new Date().toDateString();
    if (first && seen === today) return;
    // The bar is busy (the hello, someone using it): say it a little later.
    if (State.mode === "expanded") {
      window.setTimeout(() => this.weatherNews(first, true), 5000);
      return;
    }
    try { localStorage.setItem(key, today); } catch { /* fine */ }
    this.chargeView.showWeather(w);
    this.autoly(() => this.alert("charge"));
    window.setTimeout(() => {
      if (State.view === "charge" && !this.wasInIsland) this.closeBar();
    }, 3400);
  }

  private refreshLook() {
    this.applyTheme();
    let installedAt = 0;
    try {
      installedAt = Number(localStorage.getItem("cc.installedAt")) || 0;
      if (!installedAt) {
        installedAt = Date.now();
        localStorage.setItem("cc.installedAt", String(installedAt));
      }
    } catch {
      /* fine */
    }
    let now = new Date();
    // Browser preview: "&date=2026-12-25T10:00" in the address shows that day's Bob.
    const forced = !IS_TAURI ? location.hash.split("&").find((f) => f.startsWith("date="))?.slice(5) : undefined;
    if (forced && !Number.isNaN(Date.parse(forced))) now = new Date(forced);
    const season = State.settings.seasons ? lookFor(now, timeZone(), { birthday: State.settings.birthday, installedAt }) : null;
    // Dressed for the weather outside too (on its own switch).
    const w = this.weather;
    const look = season && w ? withWeather(season, w.sky, w.temp, w.day) : season;
    const changed = JSON.stringify(look) !== JSON.stringify(this.look);
    this.look = look;
    this.engine.look = look;
    // The person's own accent colour and wardrobe pick, on top of the season.
    this.engine.setAccent(State.settings.accent || null);
    this.engine.setWardrobe(State.settings.outfit);
    this.engine.setSkin(State.settings.bobSkin);
    this.ambient.kind = look && !State.settings.calm ? look.ambient : null;
    this.hideEgg();
    State.greeting = look?.greeting ?? null;
    if (changed) {
      // A special day: say so once, the first time Bob is seen that day.
      if (look?.greeting) {
        const key = `cc.greeted.${look.special}`;
        let seen = "";
        try { seen = localStorage.getItem(key) ?? ""; } catch { /* fine */ }
        if (seen !== now.toDateString()) {
          try { localStorage.setItem(key, now.toDateString()); } catch { /* fine */ }
          window.setTimeout(() => {
            if (look.special === "birthday" || look.special === "newyear" || look.special === "anniversary") this.ambient.celebrate();
            this.engine.triggerEmote("proud");
            this.showNote(look.greeting!, 4200);
          }, 4500);
        }
      }
      this.dirty = true;
      this.ensureRunning();
    }
  }

  /** A sound for something that happened in the background — never inside a full-screen app. */
  private lastPing = 0;
  private ping(name: Parameters<typeof Sound.play>[0]) {
    if (this.quiet) return;
    const now = performance.now();
    if (name !== "error" && now - this.lastPing < 1500) return; // never a burst of sounds
    this.lastPing = now;
    Sound.play(name);
  }

  private flashState(st: "finished" | "error", ms: number) {
    State.flash = st;
    if (this.flashTimer != null) window.clearTimeout(this.flashTimer);
    this.flashTimer = window.setTimeout(() => {
      this.flashTimer = null;
      State.flash = null;
      State.notify();
    }, ms);
    State.notify();
  }

  // ── Mode / view ─────────────────────────────────────────────────────────────

  private setMode(mode: IslandMode) {
    const prev = State.mode;
    if (mode === prev) return;
    State.mode = mode;
    // Opened by you: a soft open and close. Opened by itself (a card, a note): silent,
    // the card's own sound (if any) is enough.
    if (mode === "expanded") {
      // Like a drop of jelly: a squash, a stretch, and it settles.
      if (!matchMedia("(prefers-reduced-motion: reduce)").matches) {
        this.islandEl.animate(
          [{ scale: "1 1" }, { scale: "1.035 0.93", offset: 0.28 }, { scale: "0.985 1.03", offset: 0.6 }, { scale: "1.005 0.995", offset: 0.82 }, { scale: "1 1" }],
          { duration: 620, easing: "ease-out" },
        );
      }
      this.autoOpened = this.auto;
      if (!this.auto) Sound.play("open");
    }
    if (prev === "expanded") {
      if (!this.autoOpened) Sound.play("close");
      this.autoOpened = false;
      State.isPinned = false;
    }
    if (mode !== "expanded") this.engine.resetMorph();
    this.updateWindowCollapsed();
    this.animateGeometry(modeOrder(mode) < modeOrder(prev));
    State.notify();
  }

  expand(view: IslandViewName) {
    State.view = view;
    if (State.mode !== "expanded") this.setMode("expanded");
    else this.animateGeometry(false);
    State.lastActivity = performance.now();
    State.notify();
  }

  setView(view: IslandViewName) {
    if (State.mode !== "expanded") {
      this.fsm.forceHome();
      State.view = view;
      this.animateGeometry(false);
      State.notify();
      return;
    }
    if (view !== "notes") this.notesView?.stopWriting();
    if (view !== "game") this.gameBobX = null;
    State.view = view;
    State.lastActivity = performance.now();
    this.animateGeometry(false);
    State.notify();
    this.applyTint();
    if (view === "media") this.mediaTick();
  }

  collapse() {
    this.closeBar();
  }

  /** Open straight to a view (tray click, drag over, a notice). */
  alert(view: IslandViewName) {
    this.fsm.pinned = State.isPinned;
    this.fsm.forceHome();
    this.expand(view);
  }

  /** A full-screen app is in front: stay hidden unless asked. */
  private quiet = false;
  private launchedAt = performance.now();
  private awaySince = new Map<string, number>();
  private offlineTimers = new Map<string, number>();

  setQuiet(q: boolean) {
    if (q === this.quiet) return;
    this.quiet = q;
    if (q) {
      // The song strip never sits over a game or film; it comes back afterwards.
      if (State.view === "nowplaying" && State.mode === "expanded") {
        this.closeBar();
        State.notify();
        return;
      }
      // A bar the person opened on purpose (pinned) wins over a full-screen app: leave it up.
      if (State.isPinned) {
        State.notify();
        return;
      }
      State.fileDragOver = false;
      this.hintActive = false;
      this.fsm.forceHidden();
    } else {
      // The game or film closed: the song strip can come back.
      window.setTimeout(() => this.popNowPlaying(), 800);
    }
    if (!q && this.focusDonePending) {
      // The focus ended during a game or a film: say so now.
      this.focusDonePending = false;
      window.setTimeout(() => this.focusDone(), 800);
    }
    State.notify();
  }

  // ── Focus timer ─────────────────────────────────────────────────────────────

  private focusTimer: number | null = null;
  private focusDonePending = false;

  private focusStart(min: number) {
    try {
      const starts: string[] = JSON.parse(localStorage.getItem("cc.focusStarts") ?? "[]");
      starts.push(new Date().toISOString());
      localStorage.setItem("cc.focusStarts", JSON.stringify(starts.slice(-30)));
    } catch { /* fine */ }
    this.moodUp(1);
    const total = min * 60_000;
    State.focus = { endsAt: Date.now() + total, total };
    saveFocus(State.focus);
    Sound.play("approve");
    this.engine.triggerEmote("happy");
    this.focusTick();
    // Started: the bar gets out of the way; the line at the top shows the time going.
    window.setTimeout(() => this.closeBar(), 900);
    State.notify();
  }

  private focusStop() {
    State.focus = null;
    saveFocus(null);
    this.focusTick();
    State.notify();
  }

  /** Once a second while a focus runs: the clock, the line, and the end. */
  private focusTick() {
    if (this.focusTimer != null) {
      window.clearTimeout(this.focusTimer);
      this.focusTimer = null;
    }
    const f = State.focus;
    if (!f) return;
    if (Date.now() >= f.endsAt) {
      State.focus = null;
      saveFocus(null);
      if (this.quiet) this.focusDonePending = true;
      else this.focusDone();
      State.notify();
      return;
    }
    if (State.view === "focus" && State.mode === "expanded") State.notify();
    else this.syncLine();
    this.focusTimer = window.setTimeout(() => this.focusTick(), 1000);
  }

  // ── A new version ───────────────────────────────────────────────────────────

  private updateView!: ReturnType<typeof buildUpdate>;

  /** A newer version is ready: Bob says so once per version, when nothing else is on. */
  /** Just updated to a version with new features: Bob says so once, and offers the tour. */
  whatsNew(current: string) {
    const things = pending(current);
    if (!things.length) return;
    const key = `cc.wnNudged.${current}`;
    try {
      if (localStorage.getItem(key)) return;
      localStorage.setItem(key, "1");
    } catch {
      return;
    }
    const show = () => {
      if (this.quiet || State.focus || State.mode === "expanded") {
        window.setTimeout(show, 30_000);
        return;
      }
      this.updateView.showNew(current, things[0].title, things.length);
      this.engine.triggerEmote("proud");
      Sound.play("tink");
      this.autoly(() => this.alert("update"));
    };
    // Not over the hello: a little after start.
    window.setTimeout(show, 9000);
  }

  updateReady(u: UpdateInfo) {
    this.whatsNew(u.current);
    if (!u.version) return;
    const key = `cc.update.${u.version}`;
    try {
      if (localStorage.getItem(key)) return;
      localStorage.setItem(key, "1");
    } catch {
      /* fine */
    }
    const show = () => {
      if (this.quiet || State.focus || State.mode === "expanded") {
        window.setTimeout(show, 60_000);
        return;
      }
      this.updateView.show(u);
      this.engine.triggerEmote("surprised");
      Sound.play("tink");
      this.autoly(() => this.alert("update"));
    };
    show();
  }

  // ── Bob's tip of the day ────────────────────────────────────────────────────

  private readonly startedAt = Date.now();

  /** Once a day, ten minutes in, when the bar is at rest and nothing else is going on. */
  private maybeTip() {
    if (!State.settings.tips || this.quiet || State.focus || State.mode !== "hidden") return;
    if (Date.now() - this.startedAt < 10 * 60_000) return;
    const hour = new Date().getHours();
    if (hour >= 23 || hour < 8 || this.momentTimer != null) return;
    const tip = nextTip(new Date(), { mac: navigator.userAgent.includes("Mac") });
    if (!tip) return;
    this.engine.triggerEmote("wink");
    this.autoly(() => this.showNote(tip, 7000));
  }

  /** A running focus fills the line at the top as the time goes. */
  private syncLine() {
    const f = State.focus;
    if (f) {
      const done = Math.min(1, Math.max(0, 1 - (f.endsAt - Date.now()) / f.total));
      this.lineEl.dataset.focus = "on";
      this.lineEl.style.setProperty("--fp", `${(done * 100).toFixed(1)}%`);
    } else if (this.lineEl.dataset.focus) {
      delete this.lineEl.dataset.focus;
    }
  }

  private focusDone() {
    Sound.play("finish");
    this.engine.gesture("trophy", 4);
    this.ambient.celebrate();
    this.ensureRunning();
    this.showNote("Focus done! Nice work. Time for a short break.", 5200);
  }

  /** Ctrl+Alt+Z / Ctrl+Alt+U (and the tray entries). */
  private notesView: ReturnType<typeof buildNotes> | null = null;

  /** The notes changed in another window (or another copy of the bar). */
  reloadNotes() {
    State.notes = loadNotes();
    if (State.dueNote && !State.notes.some((n) => n.id === State.dueNote && !n.done)) State.dueNote = null;
    State.notify();
  }

  /** Every 15 s: a reminder whose time has come pops up (once; snooze or done answers it). */
  private reminderLoop() {
    const check = () => {
      const due = nextReminder(State.notes.filter((n) => n.remindAt! <= Date.now()));
      if (due && State.dueNote !== due.id && !this.quiet) {
        State.dueNote = due.id;
        // Shown once: clear the time so it doesn't come back after a restart.
        State.notes = updateNote(due.id, { remindAt: null });
        Sound.play("approve");
        State.isPinned = true;
        this.autoly(() => this.alert("notes"));
        State.notify();
      }
    };
    check();
    window.setInterval(check, 15_000);
  }

  /** Windows says what is playing (about once a second when something changes). */
  onMedia(m: NowPlaying | null) {
    const before = State.media;
    State.media = m;
    State.mediaAt = performance.now();
    if (this.onVideo(m, before)) return;
    if (m?.art !== before?.art) {
      const art = m?.art ?? "";
      void pictureColor(art).then((c) => {
        if ((State.media?.art ?? "") !== art) return;
        State.mediaColor = c;
        this.applyTint();
      });
    }
    // A song started (a new one, or the same one again after a pause of a minute or more).
    const started = !!m?.playing && (!before || before.title !== m.title || before.artist !== m.artist || (!before.playing && performance.now() - this.pausedAt > 60_000));
    if (before?.playing && !m?.playing) this.pausedAt = performance.now();
    if (started && this.npHiddenFor !== this.songKey()) this.npHiddenFor = "";
    if (started) this.popNowPlaying();
    if (!m && State.view === "nowplaying") this.closeBar();
    this.fetchLyrics();
    State.notify();
    this.mediaTick();
  }

  /** The video the person closed in the bar (it doesn't come back until another one). */
  private videoClosedFor = "";
  private videoLooking = "";
  private videoPausedAt = 0;

  /** A YouTube video's browser was minimised while it played: it plays on in the bar.
   *  The browser is paused; brought back, it carries on from where the bar got to.
   *  True while the bar has the video (the rest of onMedia waits). */
  private onVideo(m: NowPlaying | null, before: NowPlaying | null): boolean {
    const v = State.video;
    if (v) {
      // The browser window is back (or the video is gone): it takes over again.
      if (!m || m.title !== v.title || !m.video_hidden) this.endVideo(!!m && m.title === v.title);
      // Still playing in the minimised browser too (the first pause missed): pause it again.
      else if (m.playing && /chrome|edge|firefox|brave|opera|vivaldi/i.test(m.app) && !/webview|cubecursor/i.test(m.app) && performance.now() - this.videoPausedAt > 1500) {
        this.videoPausedAt = performance.now();
        void call("media_control", { action: "pause" }).catch(() => {});
      }
      return !!State.video;
    }
    if (!m?.video_hidden || before?.video_hidden || !m.playing || !State.settings.videoPopup) return false;
    if (this.quiet || this.videoClosedFor === m.title || this.videoLooking === m.title) return false;
    const title = m.title;
    const at = m.position_ms;
    this.videoLooking = title;
    void call<string | null>("media_video_id", { title, artist: m.artist })
      .then((id) => {
        this.videoLooking = "";
        const now = State.media;
        if (!id || !now || now.title !== title || !now.video_hidden) return;
        const startMs = now.playing ? mediaPosition() : at;
        // The browser pauses; the bar plays.
        this.videoPausedAt = performance.now();
        void call("media_control", { action: "pause" }).catch(() => {});
        State.video = { id, title, startMs, nowMs: startMs };
        this.openVideoWindow();
        State.notify();
      })
      .catch(() => { this.videoLooking = ""; });
    return false;
  }

  private openVideoWindow() {
    const v = State.video;
    if (!v) return;
    const peer = State.connectedPeers.find((p) => !p.fleet);
    void call("video_window_open", { id: v.id, title: v.title, ms: v.startMs, peer: peer?.id ?? null, peerName: peer?.name ?? null }).catch((e) => this.showNote(String(e), 3000, "home"));
  }

  /** The floating video window's buttons. */
  onVideoAction(e: { action: string; ms: number; peer?: string }) {
    const v = State.video;
    if (!v) return;
    if (e.action !== "gone") v.nowMs = e.ms;
    if (e.action === "time") return;
    Sound.play("blip");
    if (e.action === "back") {
      void call("media_show_video", { title: v.title }).catch(() => {});
      this.endVideo(true);
    } else if (e.action === "send" && e.peer) {
      const peer = State.connectedPeers.find((p) => p.id === e.peer);
      fire("ai_relay", { kind: "ui:video", json: JSON.stringify({ id: v.id, title: v.title, ms: v.nowMs }), to: e.peer });
      this.videoClosedFor = v.title;
      this.endVideo(false);
      this.paperPlane(true, /left/i.test(peer?.side ?? "") ? -1 : 1);
      this.autoly(() => this.showNote(`📺 Playing on ${peer?.name ?? "your other computer"}`, 2600));
    } else {
      // Its close button, or the window went away some other way.
      this.videoClosedFor = v.title;
      this.endVideo(false);
    }
  }

  /** The bar lets go of the video; `resume`: the browser carries on from where it got to. */
  private endVideo(resume: boolean) {
    const v = State.video;
    if (!v) return;
    State.video = null;
    void call("video_window_close").catch(() => {});
    void call("media_control", { action: `seek:${v.nowMs}` })
      .catch(() => {})
      .then(() => { if (resume) void call("media_control", { action: "play" }).catch(() => {}); });
    State.notify();
  }

  private pausedAt = 0;
  private npTimer: number | null = null;
  /** The song the person hid the strip for ("title|artist"); it comes back with the next. */
  private npHiddenFor = "";

  private songKey(): string {
    return State.media ? `${State.media.title}|${State.media.artist}` : "";
  }

  private toggleLyrics() {
    State.settings.lyrics = !State.settings.lyrics;
    State.saveSettings();
    if (State.lyricsFor.endsWith("|offline")) State.lyricsFor = "";
    this.fetchLyrics();
    State.notify();
    this.mediaTick();
  }

  /** The strip belongs on screen: music is on (or paused under a minute), it wasn't hidden
   *  for this song, and nothing full-screen or a focus wants the bar out of the way. */
  private wantsStrip(): boolean {
    const m = State.media;
    if (!m || !State.settings.mediaPopup || this.quiet || State.focus) return false;
    if (this.npHiddenFor === this.songKey()) return false;
    return m.playing || performance.now() - this.pausedAt < 60_000;
  }

  /** The slim song strip. It stays while the music plays; × hides it until the next song. */
  private popNowPlaying() {
    if (!this.wantsStrip()) return;
    if (State.mode === "expanded" && State.view !== "nowplaying") return;
    State.isPinned = true;
    this.autoly(() => this.alert("nowplaying"));
    this.applyTint();
    this.mediaTick();
    // Paused for a minute, or the music stopped: the strip goes.
    if (this.npTimer != null) window.clearInterval(this.npTimer);
    this.npTimer = window.setInterval(() => {
      if (State.view !== "nowplaying" || State.mode !== "expanded") {
        if (this.npTimer != null) window.clearInterval(this.npTimer);
        this.npTimer = null;
        return;
      }
      if (!this.wantsStrip() && !this.wasInIsland) this.closeBar();
    }, 2000);
  }

  private tint = "";
  /** The bar's colour: the album art while a song is on show, else the accent colour. */
  private applyTint() {
    const songShows = State.view === "media" || State.view === "nowplaying";
    const c = songShows && State.mediaColor ? State.mediaColor : State.settings.accent;
    if (c === this.tint) return;
    this.tint = c;
    const root = document.documentElement.style;
    if (c) root.setProperty("--bar-tint", c);
    else root.removeProperty("--bar-tint");
  }

  private mediaTimer: number | null = null;
  /** While the song shows and plays, move the progress and lyric along: every frame while
   *  lyrics are sung (the words fill smoothly), four times a second otherwise. */
  private mediaTick() {
    if (this.mediaTimer != null) return;
    const later = (f: () => void) => {
      if (State.settings.lyrics && Array.isArray(State.lyrics)) this.mediaTimer = -requestAnimationFrame(f) - 1;
      else this.mediaTimer = window.setTimeout(f, 250);
    };
    const tick = () => {
      this.mediaTimer = null;
      const v = State.view;
      if ((v !== "media" && v !== "nowplaying") || State.mode !== "expanded" || !State.media?.playing) return;
      this.views.get(v)?.sync();
      later(tick);
    };
    later(tick);
  }

  private fetchLyrics() {
    const m = State.media;
    if (!m || !State.settings.lyrics) return;
    const key = `${m.title}|${m.artist}`;
    if (key === State.lyricsFor || `${key}|offline` === State.lyricsFor) return;
    State.lyricsFor = key;
    State.lyrics = null;
    void call<{ synced: boolean; text: string } | null>("media_lyrics", { title: m.title, artist: m.artist, album: m.album, durationMs: m.duration_ms })
      .then((l) => {
        if (State.lyricsFor !== key) return;
        State.lyrics = !l ? null : l.synced ? parseLrc(l.text) : m.duration_ms ? spreadLyrics(l.text, m.duration_ms) : l.text;
        State.notify();
      })
      .catch(() => {
        // Offline or the library is down: say so; turning lyrics off and on tries again.
        if (State.lyricsFor !== key) return;
        State.lyricsFor = `${key}|offline`;
        State.notify();
      });
  }

  private lastHotkey = { what: "", at: -Infinity };
  hotkey(what: "bar" | "pill") {
    // The same press can arrive twice (sent to the island and to the settings window, and
    // a global listen() hears both): a second copy right after would close what just opened.
    const t = performance.now();
    if (this.lastHotkey.what === what && t - this.lastHotkey.at < 300) return;
    this.lastHotkey = { what, at: t };
    this.quiet = false;
    if (what === "bar") {
      // The control bar: opens, or closes again if it is already up.
      if (this.fsm.state === "home" && State.mode === "expanded" && State.view !== "nowplaying") this.closeBar();
      else {
        // The person asked for the bar on purpose: pin it so it stays until they close it,
        // even over a maximised or full-screen app (which otherwise reports "quiet" and the
        // bar would collapse the instant it opened).
        State.isPinned = true;
        this.alert("home");
      }
      return;
    }
    // The transfer bar: a toggle, independent of whether a transfer is running.
    if (this.fsm.state === "petit") {
      this.pillPinned = false;
      this.pillSuppressed = true;
      this.fsm.forceHidden();
    } else {
      this.pillSuppressed = false;
      this.pillPinned = true;
      this.fsm.forcePetit();
    }
    State.notify();
  }

  private closeBar() {
    this.notesView?.stopWriting();
    // Something else had the bar (a card, a note, the control bar): the song strip returns.
    const strip = State.view !== "nowplaying" && this.wantsStrip();
    if (strip) window.setTimeout(() => { if (State.mode !== "expanded") this.popNowPlaying(); }, 900);
    window.setTimeout(() => this.applyTint(), 0);
    State.isPinned = false;
    this.fsm.pinned = false;
    this.fsm.forceIdle();
  }

  /** Something happened: bring the transfer pill out for a moment. */
  reveal() {
    if (this.quiet || this.pillSuppressed) return;
    this.autoly(() => this.fsm.reveal());
  }

  /** True while the bar moves on its own (not because of the person). */
  private auto = false;
  private autoOpened = false;
  private autoly(f: () => void) {
    const was = this.auto;
    this.auto = true;
    try { f(); } finally { this.auto = was; }
  }

  /** Space was pressed while holding a file (true), or the drag ended (false). */
  dragHint(on: boolean) {
    if (this.quiet) return;
    if (on) {
      this.hintActive = true;
      this.cancelDropClose();
      this.onDragDrop({ type: "enter" });
      return;
    }
    this.hintActive = false;
    // The drag is over. If it ended on the drop view the drop event is on its way:
    // give it a moment, then close whatever is still open — dropping the file back
    // where it came from simply closes the bar again.
    this.scheduleDropClose(350);
  }

  /** A short message in the bar (also used by the Rust side). */
  note(msg: string) {
    this.showNote(msg, 3200);
  }

  private scheduleDropClose(ms: number) {
    this.cancelDropClose();
    this.dropCloseTimer = window.setTimeout(() => {
      this.dropCloseTimer = null;
      if (performance.now() - this.lastDropAt < 1200) return;
      if (State.fileDragOver) {
        State.fileDragOver = false;
        this.engine.animateMorph(0);
      }
      if (State.view === "drop" && this.fsm.state === "home") this.closeBar();
    }, ms);
  }

  private cancelDropClose() {
    if (this.dropCloseTimer != null) window.clearTimeout(this.dropCloseTimer);
    this.dropCloseTimer = null;
  }

  private showNote(msg: string, ms = 2600, then: "close" | "home" = "close") {
    // Never over a full-screen game or film.
    if (this.quiet) return;
    State.notice = msg;
    this.autoly(() => this.alert("note"));
    if (this.noteTimer != null) window.clearTimeout(this.noteTimer);
    this.noteTimer = window.setTimeout(() => {
      this.noteTimer = null;
      if (State.view !== "note") return;
      if (then === "home") this.setView("home");
      else this.closeBar();
    }, ms);
  }

  // ── File drop ───────────────────────────────────────────────────────────────

  private onDragDrop(e: DragDropPayload) {
    switch (e.type) {
      case "enter":
      case "over": {
        this.cancelDropClose();
        // With several computers, the one under the pointer lights up.
        const t = e.target ?? null;
        if (t !== State.dropTarget) {
          State.dropTarget = t;
          State.notify();
        }
        if (State.fileDragOver) return;
        State.fileDragOver = true;
        this.engine.animateMorph(1); // Bob turns into a box
        this.alert("drop");
        break;
      }
      case "leave": {
        State.dropTarget = null;
        if (!State.fileDragOver) return;
        State.fileDragOver = false;
        this.engine.animateMorph(0);
        State.notify();
        // Dragged off the bar without Space: close it again shortly. With Space the
        // bar stays until the button is released (see dragHint).
        if (!this.hintActive) this.scheduleDropClose(500);
        break;
      }
      case "drop": {
        this.lastDropAt = performance.now();
        this.cancelDropClose();
        State.fileDragOver = false;
        State.dropTarget = null;
        const dropped = e.drop;
        if (!dropped?.count) {
          this.engine.animateMorph(0);
          this.closeBar();
          return;
        }
        if (!State.connected) {
          this.engine.animateMorph(0);
          Sound.play("error");
          this.showNote("Connect to a computer first — then drop again.");
          return;
        }
        if (State.connectedPeers.length > 1 && !e.target) {
          // Several computers and none picked: ask who gets it (the desk ones ticked).
          this.engine.animateMorph(0);
          this.pendingDrop = dropped;
          const desk = State.connectedPeers.filter((p) => !p.fleet).map((p) => p.id);
          State.picked = new Set(desk);
          State.isPinned = true;
          this.setView("recipients");
          return;
        }
        // Shift held: don't send yet — open the "prepare before sending" menu.
        if (e.shift) {
          this.pendingDrop = dropped;
          this.pendingTarget = e.target;
          State.dropMenu = true;
          this.alert("drop");
          State.notify();
          return;
        }
        this.swallow(dropped, e.target);
        break;
      }
    }
  }

  /** Bob eats the file(s); the transfer itself runs in Rust. */
  private swallow(dropped: Dropped, target?: string, action?: "asis" | "zip" | "png" | "jpg") {
    State.staging++;
    this.pillSuppressed = false;
    dropped
      .send(target, action)
      .catch(() => {
        Sound.play("error");
        this.showNote("Couldn't read that file.");
      })
      .finally(() => {
        State.staging = Math.max(0, State.staging - 1);
        State.notify();
      });
    this.engine.gulp();
    Sound.play("approve");
    this.engine.triggerEmote("happy");
    this.engine.animateMorph(0);
    this.ensureRunning();
    // Let the gulp play, then shrink to the small bar with the progress.
    window.setTimeout(() => {
      if (State.view === "drop" && this.fsm.state === "home") this.closeBar();
    }, 750);
  }

  // ── Geometry ────────────────────────────────────────────────────────────────

  private targetSize(): { w: number; h: number; r: number } {
    const { w, h } = islandSize(State.mode, State.view, State.transferRows);
    const r = State.mode === "expanded" ? EXPANDED_CORNER : ROUNDED_CORNER;
    return { w, h, r };
  }

  private animateGeometry(shrinking: boolean) {
    const { w, h, r } = this.targetSize();
    if (shrinking) {
      this.width.curveTowards(w);
      this.height.curveTowards(h);
      this.radius.curveTowards(r);
    } else {
      this.width.springTo(w, 0.46, 0.66);
      this.height.springTo(h, 0.5, 0.7);
      this.radius.springTo(r, 0.5, 0.8);
    }
    this.ensureRunning();
  }

  private applyGeometry() {
    const w = this.width.value;
    const hh = this.height.value;
    const r = this.radius.value;
    this.islandEl.style.width = `${w}px`;
    this.islandEl.style.height = `${hh}px`;
    this.islandEl.style.borderRadius = `0 0 ${r}px ${r}px`;
    this.islandEl.style.transform = `translateX(-50%)`;
    this.placeBackdrop();
    this.greetingCanvas.style.left = `${(w - EXPANDED_W) / 2}px`;
    this.compactLabel.style.top = `${hh / 2 - 9}px`;

    // A retracted island takes no mouse at all: nothing at the top of the screen is covered.
    const rect = hh < 1 ? { x: 0, y: 0, w: 0, h: 0 } : { x: (PANEL_W - w) / 2, y: 0, w, h: hh };
    const p = this.pushedRect;
    if (Math.abs(p.x - rect.x) > 0.5 || Math.abs(p.w - rect.w) > 0.5 || Math.abs(p.h - rect.h) > 0.5) {
      this.pushedRect = rect;
      fire("set_island_rect", { x: rect.x, y: rect.y, width: rect.w, height: rect.h });
    }
  }

  // ── Liquid Glass ────────────────────────────────────────────────────────────
  //
  // The app sends small raw pictures of the screen behind the open bar (a quarter of its
  // size, window coordinates) as often as the screen refreshes, over a binary channel.
  // Each is drawn on the next frame into a small canvas the size of the picture, which
  // the GPU scales up, blurs and saturates under the bar's content; a second canvas,
  // slightly magnified, shows along the edges, where real glass bends the light. The
  // words switch between dark and light with what is behind.

  private bdEl!: HTMLCanvasElement;
  private bdEdgeImg!: HTMLCanvasElement;
  private bd: { x: number; y: number; w: number; h: number } | null = null;
  private bdPending: BackdropFrame | null = null;
  private bdFrames = 0;
  private tone: "light" | "dark" = "light";

  /** A new picture of what is behind the bar: drawn on the next frame. */
  setBackdropFrame(f: BackdropFrame) {
    if (State.settings.theme === "dark") return;
    const first = this.bdPending == null;
    this.bdPending = f;
    if (first) requestAnimationFrame(() => this.drawBackdrop());
  }

  private drawBackdrop() {
    const f = this.bdPending;
    this.bdPending = null;
    if (!f || State.settings.theme === "dark") return;
    for (const c of [this.bdEl, this.bdEdgeImg]) {
      if (c.width !== f.pw || c.height !== f.ph) {
        c.width = f.pw;
        c.height = f.ph;
      }
    }
    const x = this.bdEl.getContext("2d");
    const ex = this.bdEdgeImg.getContext("2d");
    if (!x || !ex) return;
    x.putImageData(new ImageData(f.rgba, f.pw, f.ph), 0, 0);
    ex.drawImage(this.bdEl, 0, 0);
    if (this.bdFrames++ % 6 === 0) this.readTone(f.rgba);
    if (!IS_TAURI) (window as unknown as { __glassFrames?: number }).__glassFrames = this.bdFrames;
    this.bd = { x: f.x, y: f.y, w: f.w, h: f.h };
    if (document.documentElement.dataset.liquid !== "on") document.documentElement.dataset.liquid = "on";
    this.placeBackdrop();
  }

  /** Window coordinates → the bar's own, every frame (the bar moves and grows). */
  private placeBackdrop() {
    const b = this.bd;
    if (!b || !this.bdEl) return;
    const r = this.islandEl.getBoundingClientRect();
    const x = b.x - r.left, y = b.y - r.top;
    for (const el of [this.bdEl, this.bdEdgeImg]) {
      el.style.left = `${x}px`;
      el.style.top = `${y}px`;
      el.style.width = `${b.w}px`;
      el.style.height = `${b.h}px`;
    }
    // The bend: magnify around the middle of the bar.
    this.bdEdgeImg.style.transformOrigin = `${r.width / 2 - x}px ${r.height / 2 - y}px`;
  }

  /** Bright behind the bar: dark words. Dark behind it: light words (with some give). */
  private readTone(px: Uint8ClampedArray) {
    let sum = 0, n = 0;
    // Every 13th pixel is plenty for "is it bright".
    for (let i = 0; i < px.length; i += 4 * 13) {
      sum += (0.2126 * px[i] + 0.7152 * px[i + 1] + 0.0722 * px[i + 2]) / 255;
      n++;
    }
    if (!n) return;
    const lum = sum / n;
    const tone = State.settings.barColor === "carbon" || State.settings.barColor === "midnight" ? "dark" : this.tone === "light" ? (lum < 0.5 ? "dark" : "light") : lum > 0.6 ? "light" : "dark";
    if (tone !== this.tone) {
      this.tone = tone;
      document.documentElement.dataset.tone = tone;
      this.greeting.light = tone === "light";
    }
  }

  /** Where the bar is (window coordinates), with room for the blur, while it is open. */
  glassRect(): { x: number; y: number; w: number; h: number } | null {
    if (State.settings.theme === "dark" || State.mode === "hidden") return null;
    const r = this.islandEl.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return null;
    const m = 24;
    return { x: Math.max(0, r.left - m), y: Math.max(0, r.top - m), w: r.width + 2 * m, h: r.height + 2 * m };
  }

  private islandRect(): { x: number; y: number; w: number; h: number } {
    return { x: (PANEL_W - this.width.value) / 2, y: 0, w: this.width.value, h: this.height.value };
  }

  // ── Window collapse (hidden → tiny wake strip, zero polling) ────────────────

  private updateWindowCollapsed() {
    if (this.collapseTimer != null) {
      window.clearTimeout(this.collapseTimer);
      this.collapseTimer = null;
    }
    if (State.mode === "hidden") {
      // Let the island finish retracting, then shrink the window to the wake strip:
      // from there the OS delivers no cursor events, so nothing polls at all.
      this.collapseTimer = window.setTimeout(() => {
        this.collapseTimer = null;
        if (State.mode !== "hidden") return;
        this.collapsed = true;
        fire("set_collapsed", { collapsed: true });
      }, 420);
    } else if (this.collapsed) {
      this.collapsed = false;
      fire("set_collapsed", { collapsed: false });
    }
  }

  // ── Input ───────────────────────────────────────────────────────────────────

  private wireInput() {
    // A soft light follows the cursor across the glass.
    this.islandEl.addEventListener("mousemove", (e) => {
      const r = this.islandEl.getBoundingClientRect();
      this.islandEl.style.setProperty("--mx", `${e.clientX - r.left}px`);
      this.islandEl.style.setProperty("--my", `${e.clientY - r.top}px`);
    });
    this.islandEl.addEventListener("mousedown", (e) => {
      Sound.resume();
      State.lastActivity = performance.now();
      if (State.mode !== "expanded") {
        this.fsm.click();
        return;
      }
      if (this.isBotHit(e.clientX, e.clientY)) {
        this.cancelBotHover();
        this.engine.boop();
      }
    });

    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && State.mode === "expanded") this.collapse();
      State.lastActivity = performance.now();
    });

    void onDragDrop((e) => this.onDragDrop(e));

    // Plain browser: drive the cursor from DOM events so the island can be inspected.
    if (!IS_TAURI) window.addEventListener("mousemove", (e) => this.onCursor(e.clientX, e.clientY));
  }

  /** Cursor in window-logical coordinates. */
  onCursor(x: number, y: number) {
    State.mouse = { x, y };
    const rect = this.islandRect();
    State.mouseInIsland = { x: x - rect.x, y: y - rect.y };

    const inIsland =
      x >= rect.x - HIT_MARGIN && x <= rect.x + rect.w + HIT_MARGIN &&
      y >= rect.y - HIT_MARGIN && y <= rect.y + rect.h + HIT_MARGIN;

    if (inIsland && !this.wasInIsland) {
      if (State.mode === "expanded") this.autoOpened = false;
      if (this.fsm.state === "hello") this.greeting.hover();
      // Hovering the line never opens anything: only the hotkeys, a transfer or a drop do.
      if (this.fsm.state !== "hidden") this.fsm.mouseEntered();
    }
    if (!inIsland && this.wasInIsland) this.fsm.mouseLeft();
    this.wasInIsland = inIsland;

    // Bot hover → love
    const overBot = State.mode === "expanded" && State.stateOverride == null && this.isBotHit(x, y);
    if (overBot && !this.botHovering) this.botHoverIn(x, y);
    if (!overBot && this.botHovering) this.cancelBotHover();
    this.botHovering = overBot;
    if (this.botHovering) {
      // Stroking him: the pointer going back and forth over him a few times.
      const dx = x - this.lastPetX;
      if (Math.abs(dx) > 2) {
        const dir = Math.sign(dx);
        const t = performance.now();
        if (this.petDir && dir !== this.petDir) this.petFlips.push(t);
        this.petDir = dir;
        this.petFlips = this.petFlips.filter((f) => t - f < 1300);
        if (this.petFlips.length >= 4 && t - this.lastPet > 2600) {
          this.lastPet = t;
          this.petFlips = [];
          this.engine.pet();
        }
      }
      this.lastPetX = x;
      const d = Math.hypot(x - this.botHoverStart.x, y - this.botHoverStart.y);
      if (d > 40) {
        this.botHoverStart = { x, y };
        this.scheduleLove();
      }
    }
    this.ensureRunning();
  }

  private isBotHit(x: number, y: number): boolean {
    const rect = this.islandRect();
    const cx = rect.x + this.botCx.value;
    const cy = rect.y + this.botCy.value;
    const radius = this.botSize.value / 2;
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius * radius;
  }

  private lastHoverSound = -Infinity;
  private botHoverIn(x: number, y: number) {
    if (performance.now() / 1000 - this.lastLoveTime < 6) return;
    this.botHoverStart = { x, y };
    this.engine.blink();
    this.engine.tgEs = 1.08;
    // A tiny tick, at most every 20 s: the pointer passing over him is not an event.
    if (performance.now() - this.lastHoverSound > 20_000) {
      this.lastHoverSound = performance.now();
      Sound.play("hover");
    }
    this.scheduleLove();
  }

  private scheduleLove() {
    if (this.botHoverTimer != null) window.clearTimeout(this.botHoverTimer);
    this.botHoverTimer = window.setTimeout(() => {
      this.botHoverTimer = null;
      if (!this.botHovering || State.stateOverride != null) return;
      if (performance.now() / 1000 - this.lastLoveTime < 6) return;
      this.lastLoveTime = performance.now() / 1000;
      this.engine.triggerEmote("love");
      Sound.play("love");
    }, 1900);
  }

  private cancelBotHover() {
    if (this.botHoverTimer != null) window.clearTimeout(this.botHoverTimer);
    this.botHoverTimer = null;
    this.engine.tgEs = 1;
  }

  /** Three slaps → dizzy for a moment. */
  private handleDizzy() {
    State.stateOverride = "dizzy";
    this.engine.setState("dizzy");
    Sound.play("dizzy");
    this.showNote("Whoa — easy! I'm dizzy.", 3300, "home");
    if (this.confusedRecovery != null) window.clearTimeout(this.confusedRecovery);
    this.confusedRecovery = window.setTimeout(() => {
      this.confusedRecovery = null;
      State.stateOverride = null;
      this.engine.setState(State.effectiveState);
      this.engine.triggerEmote("happy");
      State.notify();
    }, 3300);
  }

  // ── Frame loop ──────────────────────────────────────────────────────────────

  ensureRunning() {
    if (this.running) return;
    this.running = true;
    this.lastFrame = performance.now();
    requestAnimationFrame(this.frame);
  }

  /** Minimum ms between drawn frames. A busy Bob in a corner doesn't need 60 fps —
   *  and on a slower PC the animation must never compete with the mouse. */
  private frameBudget(): number {
    if (State.mode === "hidden") return 0; // only geometry settling, already cheap
    if (State.fileDragOver || State.view === "greeting") return 0;
    return State.mode === "expanded" ? 24 : 50;
  }

  private frame = (nowMs: number) => {
    if (nowMs - this.lastFrame < this.frameBudget() - 2) {
      requestAnimationFrame(this.frame);
      return;
    }
    const dt = Math.min(0.05, (nowMs - this.lastFrame) / 1000);
    this.lastFrame = nowMs;

    this.width.step(dt, nowMs);
    this.height.step(dt, nowMs);
    this.radius.step(dt, nowMs);
    this.applyGeometry();

    if (this.dirty) {
      this.dirty = false;
      this.syncDom();
    }

    this.updateBotTargets();
    this.botCx.step(dt);
    this.botCy.step(dt);
    this.botSize.step(dt);

    const ambientOn = State.mode === "expanded" && State.view !== "greeting" && this.ambient.busy;
    this.engine.music = !!State.media?.playing && !this.quiet;
    this.engine.headband = !!State.focus;
    if (ambientOn || this.ambientPx.w) this.drawAmbient(dt, ambientOn);

    const greetingActive = State.mode === "expanded" && State.view === "greeting";
    if (greetingActive) {
      const gctx = this.greetingCanvas.getContext("2d");
      if (gctx) {
        const dpr = Math.min(2, window.devicePixelRatio || 1);
        gctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        this.greeting.draw(gctx);
      }
    } else {
      this.drawBot(dt);
    }

    // A hidden island must cost nothing: only geometry settling keeps the loop alive.
    const settling = this.width.animating || this.height.animating || this.radius.animating;
    const busy = State.mode === "hidden"
      ? settling
      : settling || !this.botCx.settled || !this.botCy.settled || !this.botSize.settled || greetingActive || this.engine.busy || ambientOn;

    if (busy) {
      requestAnimationFrame(this.frame);
    } else {
      this.running = false;
      Sound.idle();
    }
  };

  private drawAmbient(dt: number, on: boolean) {
    const w = Math.round(this.width.value), hh = Math.round(this.height.value);
    const ctx = this.ambientCanvas.getContext("2d");
    if (!ctx) return;
    if (!on) {
      ctx.clearRect(0, 0, this.ambientCanvas.width, this.ambientCanvas.height);
      this.ambientPx = { w: 0, h: 0 };
      return;
    }
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (this.ambientPx.w !== w || this.ambientPx.h !== hh) {
      this.ambientPx = { w, h: hh };
      this.ambientCanvas.width = Math.max(1, Math.round(w * dpr));
      this.ambientCanvas.height = Math.max(1, Math.round(hh * dpr));
      this.ambientCanvas.style.width = `${w}px`;
      this.ambientCanvas.style.height = `${hh}px`;
    }
    this.ambient.step(dt, w, hh);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.ambient.draw(ctx, w, hh);
  }

  private updateBotTargets() {
    const p = botPosition(State.mode, State.view);
    this.botCx.target = State.view === "game" && State.mode === "expanded" && this.gameBobX != null ? this.gameBobX : p.cx;
    this.botCy.target = p.cy;
    this.botSize.target = p.diameter / 0.6;

    const greetingActive = State.mode === "expanded" && State.view === "greeting";
    this.botCanvas.style.opacity = p.opacity > 0 && !greetingActive ? "1" : "0";

    if (State.mode === "expanded" && !greetingActive) {
      const d = p.diameter;
      const st = State.effectiveState;
      this.botGlow.style.display = "block";
      this.botGlow.style.width = `${d * 2.2}px`;
      this.botGlow.style.height = `${d * 2.2}px`;
      this.botGlow.style.left = `${this.botCx.value - d * 1.1}px`;
      this.botGlow.style.top = `${this.botCy.value - d * 1.1}px`;
      this.botGlow.style.background = `radial-gradient(circle, ${botGlowColor(st)} 0%, transparent 62%)`;
      this.botGlow.style.opacity = String(botGlowOpacity(st));
    } else {
      this.botGlow.style.display = "none";
    }
  }

  private drawBot(dt: number) {
    const size = this.botSize.value;
    const w = Math.max(1, Math.round(size));
    const hCss = w + BOT_OVERHANG;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (this.canvasPx !== w) {
      this.canvasPx = w;
      this.botCanvas.width = Math.round(w * dpr);
      this.botCanvas.height = Math.round(hCss * dpr);
      this.botCanvas.style.width = `${w}px`;
      this.botCanvas.style.height = `${hCss}px`;
    }
    this.botCanvas.style.left = `${this.botCx.value - w / 2}px`;
    this.botCanvas.style.top = `${this.botCy.value - BOT_OVERHANG / 2 - hCss / 2}px`;

    const ctx = this.botCanvas.getContext("2d");
    if (!ctx) return;

    this.engine.bodyColor = null;
    this.engine.particleOverhang = BOT_OVERHANG;
    this.engine.ambient = State.mode === "expanded";
    this.engine.lookX = this.lookX();
    this.engine.lookY = this.lookY();
    if (this.engine.morph > 0.3) {
      this.engine.slotHTarget = State.fileDragOver ? 0.2 : 0;
    } else {
      this.engine.slotHTarget = 0;
      if (this.engine.morph < 0.05) {
        this.engine.slotH = 0;
        this.engine.slotHVel = 0;
      }
    }
    this.engine.update(dt);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, hCss);
    this.engine.draw(ctx, w, hCss);
  }

  /** Where the computer in play sits, for Bob to look that way. */
  private peerLook(): { x: number; y: number } | null {
    const s = State.snap;
    if (!s || (s.control !== "remote" && s.control !== "controlled")) return null;
    const side = s.paired.find((p) => p.id === s.control_peer)?.side;
    if (!side) return null;
    const x = side.includes("Left") ? -0.95 : side.includes("Right") ? 0.95 : 0;
    const y = side.startsWith("Top") ? 0.8 : side.startsWith("Bottom") ? -0.8 : 0;
    return { x, y };
  }

  private lookX(): number {
    const p = this.peerLook();
    if (p && !this.wasInIsland) return p.x;
    const rect = this.islandRect();
    return Math.tanh((State.mouse.x - (rect.x + this.botCx.value)) / 260);
  }

  private lookY(): number {
    const p = this.peerLook();
    if (p && !this.wasInIsland) return p.y;
    return -Math.tanh((State.mouse.y - this.botCy.value) / 200);
  }

  // ── DOM sync ────────────────────────────────────────────────────────────────

  private compactText(): string {
    const s = State.snap;
    if (!s) return "";
    const active = State.activeTransfers[0];
    if (!active && State.staging > 0) return "↑ Preparing…";
    if (active && active.total) {
      const pct = Math.min(100, Math.round((active.bytes / active.total) * 100));
      return `${active.direction === "out" ? "↑" : "↓"} ${pct}%  ${active.name}`;
    }
    switch (s.control) {
      case "remote":
        return `→ ${State.controlName}`;
      case "controlled":
        return `← ${State.controlName}`;
      case "local":
        return State.connectedPeers.length > 1 ? `${State.connectedPeers.length} computers` : s.connected?.name ?? "";
      default:
        return homeText(s).title;
    }
  }

  private syncDom() {
    const expanded = State.mode === "expanded";
    const greetingActive = expanded && State.view === "greeting";

    this.contentEl.style.opacity = expanded && !greetingActive ? "1" : "0";
    this.contentEl.style.pointerEvents = expanded && !greetingActive ? "auto" : "none";
    this.greetingCanvas.style.display = greetingActive ? "block" : "none";

    this.header.sync();
    this.header.el.style.display = State.view === "moment" || State.view === "charge" || State.view === "suggest" || State.view === "tour" || State.view === "nowplaying" || State.view === "video" ? "none" : "";
    for (const [name, view] of this.views) {
      const on = name === State.view;
      view.el.classList.toggle("on", on);
      // An entrance animation holds its end state: never leave one on a hidden view.
      if (!on) view.el.classList.remove("enter");
      if (on) view.sync();
    }

    // The line: the only thing on screen while the bar is at rest. Its colour tells the
    // state at a glance.
    const sn = State.snap;
    const lineState = State.activeTransfers.length || State.staging
      ? "busy"
      : sn?.control === "remote" ? "remote"
      : sn?.control === "controlled" ? "controlled"
      : sn?.control === "local" ? "local"
      : "off";
    this.lineEl.dataset.s = lineState;
    this.lineEl.style.opacity = State.mode === "hidden" && !this.quiet ? "1" : "0";
    this.syncLine();
    // At rest the pane has no size: its rim and shadow go too.
    this.islandEl.dataset.mode = State.mode;

    this.compactLabel.textContent = this.compactText();
    this.compactLabel.style.opacity = State.mode === "compact" ? "1" : "0";
    this.compactLabel.style.left = `${40 + 20}px`;

    this.engine.setState(State.effectiveState);
  }

  applySettings() {
    this.refreshLook();
    if (`${State.settings.weather}|${State.settings.city}` !== this.weatherFor) void this.fetchWeather();
    this.applyVoice();
    Sound.setEnabled(State.settings.soundEnabled);
    Sound.setVolume(State.settings.soundVolume);
    this.fsm.homeToPetitDelay = State.settings.autoCloseInterval;
    State.notify();
  }
}

export async function bootSnapshot(island: Island) {
  const s = await call<Snapshot>("snapshot").catch(() => undefined);
  if (s) island.applySnapshot(s);
}

const isPlug = (m: Moment) => m.kind === "plug" || m.kind === "unplug" || m.kind === "full";
