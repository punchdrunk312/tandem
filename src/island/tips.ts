// Bob's tip of the day: one short line about something CubeCursor can do, so people
// find what they have. Once a day at most, in order, never the same one twice in a row.

const TIPS: { text: string; mac?: boolean; pc?: boolean }[] = [
  { text: "Tip: drag a window through the edge and watch it slide onto your other computer." },
  { text: "Tip: copy on one computer, paste on the other. Text, pictures and files." },
  { text: "Tip: pick up a file, press Space, and drop it on me to send it over." },
  { text: "Tip: turn on Second monitor in Settings, and your other computer becomes an extra screen." },
  { text: "Tip: open Your computers in the bar to see every battery, and lock or wake them." },
  { text: "Tip: running low on a laptop? I'll offer to bring its apps over to the other computer." },
  { text: "Tip: the timer button starts a focus session. I keep the cards quiet until it's done." },
  { text: "Tip: press Ctrl+Alt+Z any time to open this bar.", pc: true },
  { text: "Tip: press ⌃⌥Z any time to open this bar.", mac: true },
  { text: "Tip: Scroll Lock brings your mouse straight back if anything feels stuck.", pc: true },
  { text: "Tip: press Esc three times quickly to bring your mouse back if anything feels stuck.", mac: true },
  { text: "Tip: drag a browser tab to the edge and the page opens on the other computer." },
  { text: "Tip: tell me your birthday in Settings. I like parties." },
  { text: "Tip: changed your mind? The undo button in the bar brings the last move back." },
];

const KEY_DAY = "cc.tip.day";
const KEY_I = "cc.tip.i";

/** Today's tip, if one hasn't been shown today. Remembers that it was. */
export function nextTip(now: Date, opts: { mac: boolean }): string | null {
  const today = now.toDateString();
  let i = 0;
  try {
    if (localStorage.getItem(KEY_DAY) === today) return null;
    i = Number(localStorage.getItem(KEY_I) ?? "0") || 0;
  } catch {
    return null; // without storage it would repeat every minute
  }
  const fits = TIPS.filter((t) => (opts.mac ? !t.pc : !t.mac));
  const tip = fits[i % fits.length];
  try {
    localStorage.setItem(KEY_DAY, today);
    localStorage.setItem(KEY_I, String((i + 1) % fits.length));
  } catch {
    /* fine */
  }
  return tip.text;
}
