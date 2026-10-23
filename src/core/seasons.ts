// Bob through the year: what season it is where you are, the special days, and the time
// of day. Pure functions of the date and the time zone, so they are easy to check.

export type Season = "winter" | "spring" | "summer" | "autumn";
export type Special = "christmas" | "newyear" | "halloween" | "easter" | "birthday" | "anniversary";
export type Hat = "none" | "beanie" | "santa" | "party" | "pumpkin";
export type Ambient = "snow" | "petals" | "leaves" | "sun" | "confetti" | "lights" | "embers";
export type TimeOfDay = "morning" | "day" | "evening" | "night";

/** What Bob wears and what drifts around him. */
export interface Look {
  season: Season;
  special: Special | null;
  hat: Hat;
  scarf: boolean;
  glasses: boolean;
  flower: boolean;
  egg: boolean;
  ambient: Ambient | null;
  /** A short line for the bar, on a special day. */
  greeting: string | null;
}

/** Time zones on the southern half of the globe: there, December is summer. */
const SOUTH = [
  "Australia/", "Antarctica/", "Pacific/Auckland", "Pacific/Chatham", "Pacific/Fiji", "Pacific/Tongatapu",
  "America/Argentina", "America/Buenos_Aires", "America/Sao_Paulo", "America/Santiago", "America/Montevideo",
  "America/Asuncion", "America/La_Paz", "America/Lima", "Africa/Johannesburg", "Africa/Maputo", "Africa/Harare",
  "Africa/Windhoek", "Africa/Lusaka", "Africa/Gaborone", "Indian/Mauritius", "Indian/Reunion", "Indian/Antananarivo",
];

/** Where Easter (and for some, Christmas) follows the Orthodox calendar. */
const ORTHODOX = [
  "Europe/Athens", "Asia/Nicosia", "Asia/Famagusta", "Europe/Bucharest", "Europe/Sofia", "Europe/Belgrade",
  "Europe/Skopje", "Europe/Podgorica", "Europe/Moscow", "Europe/Kyiv", "Europe/Kiev", "Europe/Minsk",
  "Europe/Chisinau", "Asia/Tbilisi", "Europe/Volgograd", "Europe/Samara",
];
/** Orthodox churches that keep Christmas on 7 January. */
const JAN7 = ["Europe/Moscow", "Europe/Belgrade", "Europe/Podgorica", "Europe/Minsk", "Asia/Tbilisi", "Europe/Volgograd", "Europe/Samara"];

export function timeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "";
  } catch {
    return "";
  }
}

const inList = (tz: string, list: string[]) => list.some((p) => tz === p || (p.endsWith("/") ? tz.startsWith(p) : tz.startsWith(p + "/") || tz === p));

export function season(d: Date, tz: string): Season {
  const m = d.getMonth(); // 0 = January
  const north: Season = m === 11 || m <= 1 ? "winter" : m <= 4 ? "spring" : m <= 7 ? "summer" : "autumn";
  if (!inList(tz, SOUTH)) return north;
  return ({ winter: "summer", summer: "winter", spring: "autumn", autumn: "spring" } as const)[north];
}

/** Orthodox Easter Sunday (Meeus' Julian algorithm, moved to the Gregorian calendar). */
export function orthodoxEaster(year: number): Date {
  const a = year % 4, b = year % 7, c = year % 19;
  const d = (19 * c + 15) % 30;
  const e = (2 * a + 4 * b - d + 34) % 7;
  const month = Math.floor((d + e + 114) / 31); // 3 = March, 4 = April (Julian)
  const day = ((d + e + 114) % 31) + 1;
  // Julian to Gregorian: 13 days from 1900 to 2099.
  return new Date(year, month - 1, day + 13);
}

/** Western Easter Sunday (the "anonymous Gregorian" algorithm). */
export function westernEaster(year: number): Date {
  const a = year % 19, b = Math.floor(year / 100), c = year % 100;
  const d = Math.floor(b / 4), e = b % 4, f = Math.floor((b + 8) / 25), g = Math.floor((b - f + 1) / 3);
  const h = (19 * a + b - d - g + 15) % 30;
  const i = Math.floor(c / 4), k = c % 4;
  const l = (32 + 2 * e + 2 * i - h - k) % 7;
  const m = Math.floor((a + 11 * h + 22 * l) / 451);
  const month = Math.floor((h + l - 7 * m + 114) / 31);
  const day = ((h + l - 7 * m + 114) % 31) + 1;
  return new Date(year, month - 1, day);
}

const sameDay = (a: Date, b: Date) => a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
const daysBetween = (a: Date, b: Date) => Math.round((new Date(a.getFullYear(), a.getMonth(), a.getDate()).getTime() - new Date(b.getFullYear(), b.getMonth(), b.getDate()).getTime()) / 864e5);

export interface Personal {
  /** "MM-DD", or empty. */
  birthday?: string;
  /** When CubeCursor first ran here (ms). */
  installedAt?: number;
}

export function special(d: Date, tz: string, me: Personal = {}): Special | null {
  const m = d.getMonth() + 1, day = d.getDate(), hour = d.getHours();
  const mmdd = `${String(m).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
  if (me.birthday && me.birthday === mmdd) return "birthday";
  // New Year's Eve from 6 pm, and New Year's Day.
  if ((m === 12 && day === 31 && hour >= 18) || (m === 1 && day === 1)) return "newyear";
  const orthodox = inList(tz, ORTHODOX);
  const easter = orthodox ? orthodoxEaster(d.getFullYear()) : westernEaster(d.getFullYear());
  const fromEaster = daysBetween(d, easter);
  if (fromEaster >= -1 && fromEaster <= 1) return "easter";
  if (inList(tz, JAN7) ? m === 1 && day >= 6 && day <= 8 : m === 12 && day >= 24 && day <= 26) return "christmas";
  if (m === 10 && day === 31) return "halloween";
  if (me.installedAt) {
    const first = new Date(me.installedAt);
    if (first.getFullYear() < d.getFullYear() && first.getMonth() === d.getMonth() && first.getDate() === d.getDate()) return "anniversary";
  }
  return null;
}

export function timeOfDay(d: Date): TimeOfDay {
  const h = d.getHours();
  if (h < 5) return "night";
  if (h < 11) return "morning";
  if (h < 18) return "day";
  return h < 23 ? "evening" : "night";
}

export function lookFor(d: Date, tz: string, me: Personal = {}): Look {
  const s = season(d, tz);
  const sp = special(d, tz, me);
  const look: Look = {
    season: s,
    special: sp,
    hat: s === "autumn" || s === "winter" ? "beanie" : "none",
    scarf: s === "winter",
    glasses: s === "summer",
    flower: s === "spring",
    egg: false,
    ambient: ({ winter: "snow", spring: "petals", summer: "sun", autumn: "leaves" } as const)[s],
    greeting: null,
  };
  switch (sp) {
    case "christmas":
      Object.assign(look, { hat: "santa", scarf: true, glasses: false, ambient: "lights", greeting: "Merry Christmas!" });
      break;
    case "newyear":
      Object.assign(look, { hat: "party", glasses: false, ambient: "confetti", greeting: "Happy New Year!" });
      break;
    case "halloween":
      Object.assign(look, { hat: "pumpkin", scarf: false, glasses: false, ambient: "embers", greeting: "Happy Halloween!" });
      break;
    case "easter":
      Object.assign(look, { flower: true, egg: true, hat: "none", ambient: "petals", greeting: inList(tz, ORTHODOX) ? "Christos Anesti!" : "Happy Easter!" });
      break;
    case "birthday":
      Object.assign(look, { hat: "party", glasses: false, ambient: "confetti", greeting: "Happy birthday!" });
      break;
    case "anniversary": {
      const years = me.installedAt ? d.getFullYear() - new Date(me.installedAt).getFullYear() : 1;
      Object.assign(look, { hat: "party", ambient: "confetti", greeting: years === 1 ? "One year together!" : `${years} years together!` });
      break;
    }
  }
  return look;
}

/** Is it the moment the year turns (to the minute)? Bob throws confetti. */
export function isMidnightOfNewYear(d: Date): boolean {
  return d.getMonth() === 0 && d.getDate() === 1 && d.getHours() === 0 && d.getMinutes() === 0;
}

export { sameDay };
