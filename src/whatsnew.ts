// What's new, version by version: after an update, the new features are shown once as a
// short tour (in the settings window), and the bar nudges the person to look.
//
// Adding a release: put its features at the top of RELEASES under its version number.
// Anyone updating from an older version sees every release in between, newest first.

export interface NewThing {
  title: string;
  text: string;
  /** A little illustration (static, trusted markup, using the tour's art classes). */
  art: string;
}

export interface Release {
  version: string;
  things: NewThing[];
}

export const RELEASES: Release[] = [
  {
    version: "0.2.0",
    things: [
      {
        title: "Changed your mind? Undo",
        text: "For two minutes after you move an app or send files, the bar has an Undo button that puts it all back.",
        art: `<div class="art-keys"><kbd>↶ Undo</kbd></div>`,
      },
      {
        title: "A focus timer in the bar",
        text: "Click the timer in the bar to start a focus session. Bob keeps notifications quiet until it ends.",
        art: `<div class="art-ring"><i></i></div>`,
      },
      {
        title: "Liquid Glass",
        text: "The bar is clear glass now: your desktop shows through it, and its words switch between light and dark to stay readable. Prefer the classic black bar? Change it under Bar & Bob.",
        art: `<div class="art-glass"></div>`,
      },
      {
        title: "The charging chime",
        text: "Plug in your laptop and a soft chime plays right away, and the bar tells you how long until it's full.",
        art: `<div class="art-plug"><i></i></div>`,
      },
      {
        title: "Your earbuds' battery",
        text: "Connect AirPods, Galaxy Buds or any Bluetooth headset, mouse or keyboard that reports its battery, and the bar shows how charged it is. For earbuds that tell each part (AirPods on a Mac), you see each bud and the case.",
        art: `<div class="art-buds"><i>80</i><i>75</i><i>60</i></div>`,
      },
      {
        title: "Tell us what you think",
        text: "Under Account there's a feedback box. It goes straight to us, with a screenshot if you like.",
        art: `<img class="art-bob" src="/brand.png" alt="">`,
      },
    ],
  },
];

const SEEN_KEY = "cc.seenVersion";

function parse(v: string): number[] {
  return v.replace(/^v/, "").split(/[-+]/)[0].split(".").map((x) => Number(x) || 0);
}

/** Is version `a` newer than `b`? */
export function newer(a: string, b: string): boolean {
  const x = parse(a), y = parse(b);
  for (let i = 0; i < 3; i++) {
    if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) > (y[i] ?? 0);
  }
  return false;
}

function get(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** The last version whose news were seen. A fresh install has nothing to catch up on;
 * someone who used CubeCursor before this list existed had the first version. */
function seen(current: string): string {
  const s = get(SEEN_KEY);
  if (s) return s;
  const installed = Number(get("cc.installedAt")) || 0;
  const oldUser = !!get("cubecursor.tour.done") || (installed > 0 && Date.now() - installed > 3600_000);
  const v = oldUser ? "0.1.0" : current;
  markSeen(v);
  return v;
}

/** What is new since the last version seen, up to `current`, newest first. */
export function pending(current: string): NewThing[] {
  if (!current) return [];
  const s = seen(current);
  return RELEASES.filter((r) => newer(r.version, s) && !newer(r.version, current)).flatMap((r) => r.things);
}

export function markSeen(version: string) {
  try {
    localStorage.setItem(SEEN_KEY, version);
  } catch {
    /* storage unavailable: shown again next time, no harm */
  }
}
