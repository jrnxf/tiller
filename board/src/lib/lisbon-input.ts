// The Hold dialog asks for a date and a time. The owner reads every moment in
// Lisbon time (and the daemon wants an instant), so the pair is read as Lisbon
// wall time whatever zone the browser is in.

const parts = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

function wall(ms: number): number {
  const p = Object.fromEntries(parts.formatToParts(new Date(ms)).map((x) => [x.type, x.value]));
  return Date.UTC(
    Number(p.year),
    Number(p.month) - 1,
    Number(p.day),
    Number(p.hour),
    Number(p.minute),
  );
}

// "2026-11-12" and "18:00" as the UTC instant that Lisbon's clock reads so.
// Null for a pair that is not a date and a time.
export function lisbonInstant(date: string, time: string): string | null {
  const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  const t = /^(\d{2}):(\d{2})$/.exec(time);
  if (!d || !t) return null;
  const asUtc = Date.UTC(Number(d[1]), Number(d[2]) - 1, Number(d[3]), Number(t[1]), Number(t[2]));
  if (Number.isNaN(asUtc)) return null;
  // Lisbon is ahead of UTC by an hour in summer: subtract the offset at the
  // guess, then once more at the result, which settles a pair near a change.
  let instant = asUtc - (wall(asUtc) - asUtc);
  instant = asUtc - (wall(instant) - instant);
  return new Date(instant).toISOString();
}

// Lisbon's calendar date `days` after `now`, as "YYYY-MM-DD".
export function lisbonDate(now: number, days = 0): string {
  const at = new Date(wall(now) + days * 86_400_000);
  return at.toISOString().slice(0, 10);
}
