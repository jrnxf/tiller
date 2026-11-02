// Lisbon calendar facts for the Health page. The browser may sit in any zone,
// so every date here is read with `timeZone: "Europe/Lisbon"`.

const parts = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "long",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

const short = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "short",
  day: "numeric",
  month: "short",
});

export interface LisbonDay {
  weekday: string;
  // 2026-11-02
  date: string;
  // 14:05
  time: string;
  year: number;
  month: number;
  day: number;
}

export function lisbonDay(iso: string): LisbonDay {
  const found = Object.fromEntries(
    parts.formatToParts(new Date(iso)).map((p) => [p.type, p.value]),
  );
  return {
    weekday: found.weekday ?? "",
    date: `${found.year}-${found.month}-${found.day}`,
    time: `${found.hour}:${found.minute}`,
    year: Number(found.year),
    month: Number(found.month),
    day: Number(found.day),
  };
}

// The ISO week of a Lisbon date, as "W44".
export function lisbonWeekLabel(iso: string): string {
  const day = lisbonDay(iso);
  const date = new Date(Date.UTC(day.year, day.month - 1, day.day));
  // The Thursday of this week decides which year the week belongs to.
  date.setUTCDate(date.getUTCDate() + 3 - ((date.getUTCDay() + 6) % 7));
  const yearStart = Date.UTC(date.getUTCFullYear(), 0, 1);
  const week = Math.floor((date.getTime() - yearStart) / 86_400_000 / 7) + 1;
  return `W${String(week).padStart(2, "0")}`;
}

// "Mon 26 Oct".
export function lisbonShortDay(iso: string): string {
  const found = Object.fromEntries(
    short.formatToParts(new Date(iso)).map((p) => [p.type, p.value]),
  );
  return `${found.weekday} ${found.day} ${found.month}`;
}
