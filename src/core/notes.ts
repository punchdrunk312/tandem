// Bob's notepad: short notes, each of which can carry a reminder. Written, shown and
// announced in the bar; kept in localStorage so they survive a restart.

export interface Note {
  id: string;
  text: string;
  created: number;
  /** When Bob should bring it up (ms since 1970), or null. */
  remindAt: number | null;
  /** Ticked off. */
  done: boolean;
}

export const NOTES_KEY = "cc.notes";

export function loadNotes(): Note[] {
  try {
    const v = JSON.parse(localStorage.getItem(NOTES_KEY) ?? "[]");
    return Array.isArray(v) ? (v as Note[]).filter((n) => n && typeof n.text === "string") : [];
  } catch {
    return [];
  }
}

export function saveNotes(notes: Note[]) {
  try {
    localStorage.setItem(NOTES_KEY, JSON.stringify(notes.slice(0, 200)));
  } catch {
    /* storage full or unavailable: the note just isn't kept */
  }
}

export function addNote(text: string, remindAt: number | null = null): Note[] {
  const t = text.trim().slice(0, 500);
  const notes = loadNotes();
  if (!t) return notes;
  notes.unshift({ id: `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`, text: t, created: Date.now(), remindAt, done: false });
  saveNotes(notes);
  return notes;
}

export function updateNote(id: string, patch: Partial<Omit<Note, "id">>): Note[] {
  const notes = loadNotes().map((n) => (n.id === id ? { ...n, ...patch } : n));
  saveNotes(notes);
  return notes;
}

export function removeNote(id: string): Note[] {
  const notes = loadNotes().filter((n) => n.id !== id);
  saveNotes(notes);
  return notes;
}

/** The next reminder still to come (or overdue and not yet done). */
export function nextReminder(notes: Note[]): Note | null {
  return notes.filter((n) => !n.done && n.remindAt != null).sort((a, b) => a.remindAt! - b.remindAt!)[0] ?? null;
}

/** Quick picks for "remind me": a label and when, counted from `now`. */
export function reminderPresets(now = new Date()): { label: string; at: number }[] {
  const at = (h: number, dayOffset = 0) => {
    const d = new Date(now);
    d.setDate(d.getDate() + dayOffset);
    d.setHours(h, 0, 0, 0);
    return d.getTime();
  };
  const out = [
    { label: "In 10 min", at: now.getTime() + 10 * 60_000 },
    { label: "In 1 hour", at: now.getTime() + 60 * 60_000 },
  ];
  if (now.getHours() < 18) out.push({ label: "This evening", at: at(18) });
  out.push({ label: "Tomorrow 9:00", at: at(9, 1) });
  return out;
}

/** "Today 18:00", "Tomorrow 9:00", "Fri 3 Nov 14:30", or "Overdue · 14:30". */
export function whenText(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const hm = `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (ms < now) return `Overdue · ${hm}`;
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(d) - day(new Date(now))) / 86_400_000);
  if (diff === 0) return `Today ${hm}`;
  if (diff === 1) return `Tomorrow ${hm}`;
  return `${d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" })} ${hm}`;
}
