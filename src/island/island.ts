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
import { buildDrop, buildHeader, buildHome, buildNote, homeText, type ViewActions, type ViewHost } from "./views";

const BOT_OVERHANG = 40;
/** Same margin as the Rust hit test (src-tauri/src/island.rs). */
const HIT_MARGIN = 14;

const modeOrder = (m: IslandMode) => (m === "hidden" ? 0 : m === "compact" ? 1 : 2);

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
    this.viewsEl = h("div", { id: "views" });
    for (const v of this.views.values()) this.viewsEl.append(v.el);
    this.contentEl = h("div", { id: "content" }, this.header.el, this.viewsEl);

    const clip = h("div", { id: "island-clip" }, this.greetingCanvas, this.contentEl);
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
          else if (from === "hidden") Sound.play("peek");
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
    this.fsm.launch();
  }

  // ── Snapshot from Rust ──────────────────────────────────────────────────────

  applySnapshot(s: Snapshot) {
    const prev = State.snap;
    State.snap = s;
    if (prev) {
      // Connection changes don't pop anything up: the line at the top changes colour.
      if (!prev.connected && s.connected) {
        this.ping("approve");
        this.engine.triggerEmote("happy");
      } else if (prev.connected && !s.connected) {
        this.ping("blip");
      } else if (prev.control !== s.control) {
        this.ping(s.control === "remote" ? "open" : "blip");
      }
      for (const t of s.transfers) {
        const before = prev.transfers.find((p) => p.id === t.id && p.direction === t.direction);
        if (!before) {
          // A new transfer brings the pill back, even if it was closed by hand.
          this.pillSuppressed = false;
          this.reveal();
          if (t.direction === "in") this.ping("attach");
        } else if (before.state === "active" && t.state === "done") {
          this.flashState("finished", 2200);
          this.ping("finish");
          this.engine.triggerEmote("proud");
          this.reveal();
        } else if (before.state !== "error" && t.state === "error") {
          this.pillSuppressed = false;
          this.flashState("error", 3200);
          this.ping("error");
          this.reveal();
        }
      }
    }
    const rows = State.transferRows;
    if (rows !== this.lastRows) {
      this.lastRows = rows;
      if (State.mode === "expanded") this.animateGeometry(false);
    }
    State.notify();
  }

  /** A sound for something that happened in the background — never inside a full-screen app. */
  private ping(name: Parameters<typeof Sound.play>[0]) {
    if (!this.quiet) Sound.play(name);
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
    if (mode === "expanded") Sound.play("open");
    if (prev === "expanded") {
      Sound.play("close");
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

  setQuiet(q: boolean) {
    if (q === this.quiet) return;
    this.quiet = q;
    if (q) {
      State.fileDragOver = false;
      this.hintActive = false;
      this.fsm.forceHidden();
    }
    State.notify();
  }

  /** Ctrl+Alt+T / Ctrl+Alt+U (and the tray entries). */
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
    this.fsm.reveal();
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
    State.notice = msg;
    this.alert("note");
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
        this.engine.slap();
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

  private botHoverIn(x: number, y: number) {
    if (performance.now() / 1000 - this.lastLoveTime < 6) return;
    this.botHoverStart = { x, y };
    this.engine.blink();
    this.engine.tgEs = 1.08;
    Sound.play("hover");
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
      : settling || !this.botCx.settled || !this.botCy.settled || !this.botSize.settled || greetingActive || this.engine.busy;

    if (busy) {
      requestAnimationFrame(this.frame);
    } else {
      this.running = false;
      Sound.idle();
    }
  };

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

  private lookX(): number {
    const rect = this.islandRect();
    return Math.tanh((State.mouse.x - (rect.x + this.botCx.value)) / 260);
  }

  private lookY(): number {
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

    this.compactLabel.textContent = this.compactText();
    this.compactLabel.style.opacity = State.mode === "compact" ? "1" : "0";
    this.compactLabel.style.left = `${40 + 20}px`;

    this.engine.setState(State.effectiveState);
  }

  applySettings() {
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
