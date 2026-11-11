// The retros for the dev and preview servers and for tests (learning-loop.md
// section 4): kept in memory, so Run again moves a retro that did not finish
// to running and, a little later, to its result. No curator runs here: the
// stand-in run ends by the clock and files a placeholder suggestion.
import type { MemoryResponse, RetroView, RetrosResponse } from "./api-types.ts";
import { curatorSuggestion, memoryResource } from "./fixtures-memory.ts";

const TIMEOUT_MINUTES = 20;
// How long a stand-in curator runs before its retro is done.
export const RUN_MS = 40_000;
const DAY = 24 * 60;

interface State {
  retros: RetroView[];
  // The retros a stand-in curator holds, and when it ends.
  runs: Map<string, number>;
}

let state: State | null = null;

const at = (now: number, minutes: number) => new Date(now + minutes * 60_000).toISOString();

function retro(
  fields: Pick<RetroView, "id" | "card" | "title" | "project_id" | "status" | "created_at"> &
    Partial<RetroView>,
): RetroView {
  return {
    card_id: `01JCARD${fields.card.replace(/\W/g, "").toUpperCase().padStart(19, "0")}`,
    outcome: null,
    learning: null,
    curator_started_at: null,
    started_by: null,
    finished_at: null,
    proposal: null,
    run_refusal: null,
    ...fields,
  };
}

// The open suggestion `card`'s curator filed in `project`'s document.
function suggestionOf(project: string, card: string): { id: string; section: string } | null {
  const url = new URL(`/api/v1/memory?scope=${project}`, "http://localhost");
  const doc = (memoryResource(url) as MemoryResponse | undefined)?.doc;
  for (const section of doc?.sections ?? []) {
    const found = [
      ...section.lines.flatMap((line) => (line.suggestion ? [line.suggestion] : [])),
      ...section.suggestions,
    ].find((sug) => sug.card === card);
    if (found) return { id: found.proposal_id, section: section.title };
  }
  return null;
}

function seed(now: number): State {
  const cut = at(now, -11 * 60 - 20);
  const waiting = suggestionOf("beta", "beta-28");
  return {
    runs: new Map(),
    retros: [
      retro({
        id: "01JRETRO000000000000000014",
        card: "alpha-14",
        title: "Split the export worker",
        project_id: "alpha",
        status: "unfinished",
        outcome: "unfinished",
        learning: `curator cut after ${TIMEOUT_MINUTES} minutes without finishing the retro`,
        created_at: at(now, -11 * 60 - 45),
        curator_started_at: at(now, -11 * 60 - 40),
        started_by: "daemon",
        finished_at: cut,
      }),
      retro({
        id: "01JRETRO000000000000000028",
        card: "beta-28",
        title: "Run the slow suite on one worker",
        project_id: "beta",
        status: "done",
        outcome: waiting ? "proposed" : "nothing",
        created_at: at(now, -DAY - 3 * 60),
        curator_started_at: at(now, -DAY - 2 * 60 - 50),
        started_by: "daemon",
        finished_at: at(now, -DAY - 2 * 60 - 41),
        proposal: waiting ? { id: waiting.id, state: "proposed", section: waiting.section } : null,
      }),
      retro({
        id: "01JRETRO000000000000000012",
        card: "alpha-12",
        title: "Retry the export upload",
        project_id: "alpha",
        status: "done",
        outcome: "proposed",
        created_at: at(now, -DAY - 14 * 60),
        curator_started_at: at(now, -DAY - 13 * 60 - 50),
        started_by: "daemon",
        finished_at: at(now, -DAY - 13 * 60 - 38),
        proposal: { id: "01JPROP0000000000000000412", state: "applied", section: "Watch out for" },
      }),
      retro({
        id: "01JRETRO000000000000000031",
        card: "gamma-31",
        title: "Cache the font files in the image build",
        project_id: "gamma",
        status: "done",
        outcome: "evidence",
        learning: "The font cache is cold on every new runner.",
        created_at: at(now, -2 * DAY - 5 * 60),
        curator_started_at: at(now, -2 * DAY - 4 * 60 - 55),
        started_by: "daemon",
        finished_at: at(now, -2 * DAY - 4 * 60 - 47),
      }),
      retro({
        id: "01JRETRO000000000000000009",
        card: "alpha-9",
        title: "Move fixtures to one folder",
        project_id: "alpha",
        status: "done",
        outcome: "nothing",
        created_at: at(now, -5 * DAY - 2 * 60),
        curator_started_at: at(now, -5 * DAY - 60 - 55),
        started_by: "daemon",
        finished_at: at(now, -5 * DAY - 60 - 48),
      }),
    ],
  };
}

// A stand-in curator that has run its time finishes its retro with one
// suggestion, waiting for review in the document.
function settle(s: State, now: number) {
  for (const [id, doneAt] of s.runs) {
    if (now < doneAt) continue;
    s.runs.delete(id);
    const found = s.retros.find((each) => each.id === id);
    if (!found) continue;
    const finished = new Date(doneAt).toISOString();
    const section = "Watch out for";
    const proposal = curatorSuggestion(
      found.project_id,
      found.card,
      section,
      "Drain the export queue before a worker is split; a split mid-run loses the jobs in flight.",
      finished,
    );
    Object.assign(found, {
      status: "done",
      outcome: proposal ? "proposed" : "nothing",
      learning: null,
      finished_at: finished,
      proposal: proposal ? { id: proposal, state: "proposed", section } : null,
    } satisfies Partial<RetroView>);
  }
}

function current(now: number): State {
  state ??= seed(now);
  settle(state, now);
  return state;
}

// A stand-in run files its suggestion when it ends, whichever read comes
// first: the memory view reads it too.
export function settleRetros(now: number = Date.now()): void {
  current(now);
}

// Tests start each case from the seeded retros.
export function resetRetrosFixture(now: number = Date.now()): void {
  state = seed(now);
}

// Why Run again is refused on `view` now, as the daemon says it.
function refusalOf(s: State, view: RetroView): string | null {
  if (view.status === "running") return "its curator still runs";
  if (view.status === "done")
    return `retro ${view.id} is done; only an unfinished retro runs again`;
  const other = s.retros.find(
    (each) => each.id !== view.id && each.card_id === view.card_id && each.status === "running",
  );
  if (other) {
    return `another retro of ${view.card} is running (retro ${other.id}); one card has one curator at a time`;
  }
  return null;
}

export function retrosResource(url: URL, now: number = Date.now()): RetrosResponse {
  const s = current(now);
  const project = url.searchParams.get("project");
  return {
    retros: s.retros
      .filter((each) => !project || each.project_id === project)
      .map((each) => ({ ...each, run_refusal: refusalOf(s, each) })),
    curator_timeout_minutes: TIMEOUT_MINUTES,
  };
}

export type RetroReply =
  | { ok: true; data: RetroView }
  | { ok: false; status: number; code: string; message: string };

// `POST /api/v1/retros/{id}/run`, or `null` when `path` is not it.
export function retroMutation(method: string, path: string): RetroReply | null {
  const run = /^\/api\/v1\/retros\/([^/]+)\/run$/.exec(path);
  if (method !== "POST" || !run?.[1]) return null;
  const now = Date.now();
  const s = current(now);
  const id = run[1];
  const found = s.retros.find((each) => each.id === id || each.card === id);
  if (!found) return { ok: false, status: 404, code: "not_found", message: `no retro ${id}` };
  const refusal = refusalOf(s, found);
  if (refusal)
    return { ok: false, status: 422, code: "refused", message: `retro ${found.id}: ${refusal}` };
  Object.assign(found, {
    status: "running",
    outcome: null,
    learning: null,
    finished_at: null,
    curator_started_at: new Date(now).toISOString(),
    started_by: "board",
  } satisfies Partial<RetroView>);
  s.runs.set(found.id, now + RUN_MS);
  return { ok: true, data: { ...found } };
}
