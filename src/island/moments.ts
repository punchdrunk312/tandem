// The device moments: a laptop plugged in or unplugged, running low, full; a computer
// joining or leaving; a wireless mouse or headset running out. Worked out by comparing
// each snapshot with the one before, so nothing extra has to come from Rust.

import type { DeviceInfo, SideName, Snapshot } from "../types";

export type MomentKind = "plug" | "unplug" | "low" | "full" | "online" | "offline" | "accessory";

export interface Moment {
  kind: MomentKind;
  /** "me" for this computer, otherwise the other computer's id. */
  id: string;
  name: string;
  side: SideName | null;
  color: string;
  device: DeviceInfo | null;
  acc?: { name: string; battery: number };
  /** Its sound already played, the moment it happened. */
  chimed?: boolean;
}

interface Seen {
  connected: boolean;
  battery: number | null;
  plugged: boolean;
  charging: boolean;
  acc: Map<string, number>;
}

const LOW_STEPS = [20, 10, 5];
/** This computer: Windows and macOS already warn around 20%, so only the later steps. */
const LOW_STEPS_ME = [10, 5];
/** The same moment for the same computer at most once a minute. */
const COOLDOWN_MS = 60_000;
/** Plugged in and out share one: a wobbly cable shows one card, not a stream of them. */
const POWER_COOLDOWN_MS = 45_000;
/** Accessories nag once a day at most. */
const ACC_KEY = "cc.acc.";
const ACC_LOW = 15;

export class MomentWatcher {
  private seen = new Map<string, Seen>();
  private last = new Map<string, number>();
  private started = false;
  /** "Fully charged" said once per charge, until it is unplugged again. */
  private fullSaid = new Set<string>();

  diff(s: Snapshot, now = Date.now()): Moment[] {
    const out: Moment[] = [];
    const devices: { id: string; name: string; side: SideName | null; color: string; connected: boolean; d: DeviceInfo | null }[] = [
      { id: "me", name: s.device_name, side: null, color: s.my_color ?? "#2dd4bf", connected: true, d: s.me ?? null },
      ...s.paired.map((p) => ({ id: p.id, name: p.name, side: p.side, color: p.color ?? "#2dd4bf", connected: p.connected, d: p.device ?? null })),
    ];
    for (const dev of devices) {
      const before = this.seen.get(dev.id);
      const d = dev.d;
      const now_: Seen = {
        connected: dev.connected,
        battery: d?.battery ?? null,
        plugged: d?.plugged ?? false,
        charging: d?.charging ?? false,
        acc: new Map((d?.accessories ?? []).map((a) => [a.name, a.battery])),
      };
      // A computer's numbers only count once it has sent some (it was just connected).
      const hasInfo = !!d;
      if (before && this.started) {
        const base = { id: dev.id, name: dev.name, side: dev.side, color: dev.color, device: d };
        if (dev.id !== "me" && !before.connected && dev.connected) out.push({ ...base, kind: "online" });
        if (dev.id !== "me" && before.connected && !dev.connected) out.push({ ...base, kind: "offline" });
        const hadInfo = before.battery != null || before.acc.size > 0 || before.plugged;
        if (hasInfo && hadInfo && dev.connected && now_.battery != null && before.battery != null) {
          const me = dev.id === "me";
          if (!before.plugged && now_.plugged) out.push({ ...base, kind: "plug" });
          // You just pulled the cable out of this computer yourself: no card for that.
          else if (before.plugged && !now_.plugged && !me) out.push({ ...base, kind: "unplug" });
          if (!now_.plugged) {
            this.fullSaid.delete(dev.id);
            const crossed = (me ? LOW_STEPS_ME : LOW_STEPS).find((t) => before.battery! > t && now_.battery! <= t);
            if (crossed != null) out.push({ ...base, kind: "low" });
          }
          // Batteries that hover at 99–100% while plugged in would say it again and again.
          if (now_.plugged && now_.battery >= 100 && before.battery < 100 && !this.fullSaid.has(dev.id)) {
            this.fullSaid.add(dev.id);
            out.push({ ...base, kind: "full" });
          }
        }
        if (hasInfo && dev.connected) {
          for (const [name, b] of now_.acc) {
            const was = before.acc.get(name);
            if (was != null && was > ACC_LOW && b <= ACC_LOW && this.accessoryOnce(name, now)) {
              out.push({ ...base, kind: "accessory", acc: { name, battery: b } });
            }
          }
        }
      }
      // Without fresh numbers (it just went away), keep what was known.
      this.seen.set(dev.id, hasInfo || !before ? now_ : { ...before, connected: dev.connected });
    }
    this.started = true;
    return out.filter((m) => {
      const group = m.kind === "plug" || m.kind === "unplug" ? "power" : m.kind;
      const wait = group === "power" ? POWER_COOLDOWN_MS : COOLDOWN_MS;
      const k = `${group}|${m.id}|${m.acc?.name ?? ""}`;
      const t = this.last.get(k) ?? -Infinity;
      if (now - t < wait) return false;
      this.last.set(k, now);
      return true;
    });
  }

  private accessoryOnce(name: string, now: number): boolean {
    const day = new Date(now).toDateString();
    try {
      if (localStorage.getItem(ACC_KEY + name) === day) return false;
      localStorage.setItem(ACC_KEY + name, day);
    } catch {
      /* fine: then it may say so again */
    }
    return true;
  }
}

/** Where a sound about that computer comes from: -1 left … 1 right. */
export function panFor(side: SideName | null): number {
  switch (side) {
    case "Left": return -0.85;
    case "Right": return 0.85;
    case "TopLeft":
    case "BottomLeft": return -0.6;
    case "TopRight":
    case "BottomRight": return 0.6;
    default: return 0;
  }
}

export function fmtMins(m: number | null | undefined): string {
  if (m == null) return "";
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60), r = m % 60;
  return r ? `${h} h ${r} min` : `${h} h`;
}

/** The words for a moment: a title and a line under it. */
export function momentText(m: Moment): { title: string; sub: string } {
  const me = m.id === "me";
  const who = me ? "This computer" : m.name;
  const d = m.device;
  const pct = d?.battery != null ? `${d.battery}%` : "";
  const side = m.side ? m.side.replace(/([a-z])([A-Z])/, "$1 $2").toLowerCase() : "";
  switch (m.kind) {
    case "plug":
      return {
        title: `${who} is charging`,
        sub: d?.mins_to_full != null ? `${pct} · full in ${fmtMins(d.mins_to_full)}` : `${pct} · filling up`,
      };
    case "unplug":
      return { title: `${who} is on battery`, sub: d?.mins_left != null ? `${pct} · about ${fmtMins(d.mins_left)} left` : pct };
    case "low":
      return {
        title: `${who} is at ${pct}`,
        sub: me ? "Plug in soon." : "Plug it in, or bring what's open there over here.",
      };
    case "full":
      return { title: `${who} is fully charged`, sub: "You can unplug it now." };
    case "online":
      return { title: `${m.name} is here`, sub: side ? `On your ${side}. Push the cursor that way.` : "Connected." };
    case "offline":
      return { title: `${m.name} went away`, sub: "It reconnects on its own when it's back." };
    case "accessory":
      return { title: `${m.acc?.name} is at ${m.acc?.battery}%`, sub: me ? "Charge it soon." : `On ${m.name}. Charge it soon.` };
  }
}
