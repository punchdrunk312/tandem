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
import { buildDevices, buildDrop, buildFocus, buildHeader, buildHome, buildMoment, buildNote, buildUpdate, homeText, type UpdateInfo, type ViewActions, type ViewHost } from "./views";
import { saveFocus } from "./state";
import { nextTip } from "./tips";
import { MomentWatcher, panFor, type Moment } from "./moments";
import { AmbientLayer } from "./ambient";
import { isMidnightOfNewYear, lookFor, timeOfDay, timeZone, type Look } from "../core/seasons";

const BOT_OVERHANG = 40;
/** Same margin as the Rust hit test (src-tauri/src/island.rs). */
const HIT_MARGIN = 14;

const modeOrder = (m: IslandMode) => (m === "hidden" ? 0 : m === "compact" ? 1 : 2);

/** How long a computer may be gone before it counts as gone. */
const OFFLINE_GRACE_MS = 20_000;

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
    // A focus that was running when CubeCursor closed carries on.
    this.focusTick();
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
      toggleSound: () => {
        State.settings.soundEnabled = !State.settings.soundEnabled;
        Sound.setEnabled(State.settings.soundEnabled);
        State.saveSettings();
        State.notify();
      },
    };

    this.lineEl = h("div", { id: "line" });
    this.botGlow = h("div", { id: "bot-glow" });
    this.botCanvas = h("canvas", { id: "bot-canvas" });
    this.greetingCanvas = h("canvas", { id: "greeting-canvas" });
    this.compactLabel = h("div", { id: "compact-label" });

    this.header = buildHeader(actions);
    this.views.set("home", buildHome(actions));
    this.views.set("drop", buildDrop());
    this.views.set("note", buildNote());
    this.momentView = buildMoment({
      takeApps: (id) => { Sound.play("send"); fire("take_apps", { peerId: id }); this.closeBar(); },
      jump: (id) => { fire("engage", { peerId: id }); this.closeBar(); },
    });
    this.views.set("moment", this.momentView);
    this.views.set("devices", buildDevices({
      blip: () => Sound.play("blip"),
      jump: (id) => { fire("engage", { peerId: id }); this.closeBar(); },
      power: (id, action) => fire("power_peer", { peerId: id, action }),
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
    });
    this.views.set("update", this.updateView);
    this.viewsEl = h("div", { id: "views" });
    for (const v of this.views.values()) this.viewsEl.append(v.el);
    this.contentEl = h("div", { id: "content" }, this.header.el, this.viewsEl);

    this.ambientCanvas = h("canvas", { id: "ambient-canvas" });
    // Liquid Glass: what is behind the bar, blurred, and again bent along its edges.
    this.bdEl = h("div", { id: "bd" });
    this.bdEdgeImg = h("div", { id: "bd-edge-img" });
    const glass = h("div", { id: "glass" }, this.bdEl, h("div", { id: "bd-edge" }, this.bdEdgeImg));
    const clip = h("div", { id: "island-clip" }, glass, this.ambientCanvas, this.greetingCanvas, this.contentEl);
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

  launch() {
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
            this.engine.triggerEmote("proud");
          }
          this.reveal();
        } else if (before.state !== "error" && t.state === "error") {
          this.pillSuppressed = false;
          this.flashState("error", 3200);
          this.ping("error");
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
    if (!State.settings.moments) return;
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
    if (State.mode === "expanded" && State.view !== "moment" && this.wasInIsland) {
      this.momentTimer = window.setTimeout(() => { this.momentTimer = null; this.nextMoment(); }, 2500);
      return;
    }
    const m = this.momentQueue.shift();
    if (!m) return;
    State.moment = m;
    this.momentView.show(m);
    this.autoly(() => this.alert("moment"));
    this.react(m);
    const dur = m.kind === "low" && m.id !== "me" ? 8000 : m.kind === "plug" ? 4800 : 3800;
    const done = () => {
      this.momentTimer = null;
      if (State.view === "moment") {
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
        sound("plug");
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
        sound("full");
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
    }
  }

  // ── Bob's look: the season, a special day ───────────────────────────────────

  private glassOn: boolean | null = null;
  /** Liquid Glass or the classic black bar: the page, Bob and the season all follow. */
  private applyTheme() {
    const glass = State.settings.theme !== "dark";
    document.documentElement.dataset.theme = glass ? "glass" : "dark";
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
    this.greeting.light = glass && !(this.bd && this.tone === "dark");
    this.ambient.light = glass;
    this.dirty = true;
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
    const look = State.settings.seasons ? lookFor(now, timeZone(), { birthday: State.settings.birthday, installedAt }) : null;
    const changed = JSON.stringify(look) !== JSON.stringify(this.look);
    this.look = look;
    this.engine.look = look;
    this.ambient.kind = look && !State.settings.calm ? look.ambient : null;
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
    State.view = view;
    State.lastActivity = performance.now();
    this.animateGeometry(false);
    State.notify();
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
      State.fileDragOver = false;
      this.hintActive = false;
      this.fsm.forceHidden();
    } else if (this.focusDonePending) {
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
  updateReady(u: UpdateInfo) {
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
    this.engine.triggerEmote("proud");
    this.ambient.celebrate();
    this.ensureRunning();
    this.showNote("Focus done! Nice work. Time for a short break.", 5200);
  }

  /** Ctrl+Alt+Z / Ctrl+Alt+U (and the tray entries). */
  hotkey(what: "bar" | "pill") {
    this.quiet = false;
    if (what === "bar") {
      // The control bar: opens, or closes again if it is already up.
      if (this.fsm.state === "home" && State.mode === "expanded") this.closeBar();
      else this.alert("home");
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
          // Several computers and none picked: don't guess.
          this.engine.animateMorph(0);
          Sound.play("error");
          this.showNote("Drop it on a computer's name.", 2600, "home");
          return;
        }
        this.swallow(dropped, e.target);
        break;
      }
    }
  }

  /** Bob eats the file(s); the transfer itself runs in Rust. */
  private swallow(dropped: Dropped, target?: string) {
    State.staging++;
    this.pillSuppressed = false;
    dropped
      .send(target)
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
      this.width.springTo(w);
      this.height.springTo(h);
      this.radius.springTo(r);
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
  // The app photographs the screen behind the open bar about twelve times a second
  // (`backdrop` events, window coordinates). The picture is drawn under the bar's content,
  // blurred and saturated, and once more slightly magnified along the edges, where real
  // glass bends the light. The words switch between dark and light with what is behind.

  private bdEl!: HTMLElement;
  private bdEdgeImg!: HTMLElement;
  private bd: { x: number; y: number; w: number; h: number; url: string } | null = null;
  private tone: "light" | "dark" = "light";
  private toneBusy = false;

  setBackdrop(p: { x: number; y: number; w: number; h: number; url: string }) {
    if (State.settings.theme === "dark") return;
    if (p.url !== this.bd?.url) {
      const bg = `url("${p.url}")`;
      this.bdEl.style.backgroundImage = bg;
      this.bdEdgeImg.style.backgroundImage = bg;
      this.readTone(p.url);
    }
    this.bd = p;
    document.documentElement.dataset.liquid = "on";
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
  private readTone(url: string) {
    if (this.toneBusy) return;
    this.toneBusy = true;
    const img = new Image();
    img.onload = () => {
      this.toneBusy = false;
      try {
        const c = document.createElement("canvas");
        c.width = 24;
        c.height = 12;
        const x = c.getContext("2d", { willReadFrequently: true });
        if (!x) return;
        x.drawImage(img, 0, 0, 24, 12);
        const d = x.getImageData(2, 0, 20, 10).data;
        let sum = 0;
        for (let i = 0; i < d.length; i += 4) sum += (0.2126 * d[i] + 0.7152 * d[i + 1] + 0.0722 * d[i + 2]) / 255;
        const lum = sum / (d.length / 4);
        const tone = this.tone === "light" ? (lum < 0.5 ? "dark" : "light") : lum > 0.6 ? "light" : "dark";
        if (tone !== this.tone) {
          this.tone = tone;
          document.documentElement.dataset.tone = tone;
          this.greeting.light = tone === "light";
        }
      } catch {
        /* fine: keep the tone */
      }
    };
    img.onerror = () => { this.toneBusy = false; };
    img.src = url;
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
    this.botCx.target = p.cx;
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
    this.header.el.style.display = State.view === "moment" ? "none" : "";
    for (const [name, view] of this.views) {
      const on = name === State.view;
      view.el.classList.toggle("on", on);
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
