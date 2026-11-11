// The memory view for the dev and preview servers and for tests: one
// document per project plus the shared one, kept in memory so Keep, Discard,
// an edit, an undo and a holdout eval change what the next read answers
// (memory.md section 7, protocol.md sections 7.3 and 7.4). Names and lines
// are placeholders, and no eval here runs a model.
import type {
  MemoryChange,
  MemoryDocSummary,
  MemoryDocument,
  MemoryEvalJob,
  MemoryEvalPlan,
  MemoryHoldout,
  MemoryLanded,
  MemoryLine,
  MemoryProvenance,
  MemoryResponse,
  MemoryRevision,
  MemorySuggestion,
  QuotaWindow,
} from "./api-types.ts";

const FLEET = "fleet";

interface Line {
  text: string;
  memory_id: string;
  provenance: MemoryProvenance;
  suggestion: MemorySuggestion | null;
}

interface Section {
  title: string;
  class: string;
  lines: Line[];
  suggestions: MemorySuggestion[];
}

interface Doc {
  project_id: string;
  name: string;
  path: string;
  blob: string;
  dirty: boolean;
  sections: Section[];
}

// One landed change, and for a discard the suggestion an undo brings back.
interface Landed {
  project_id: string;
  change: MemoryChange;
  discarded?: MemorySuggestion;
}

// A holdout eval the board started, and when the stand-in runner ends it.
interface Job {
  view: MemoryEvalJob;
  doneAt: number;
}

interface State {
  docs: Doc[];
  landed: Landed[];
  revisions: Map<string, MemoryRevision>;
  jobs: Job[];
  // The change set each suggestion is in, by name.
  sets: Map<string, string>;
  counter: number;
}

// The stand-in holdout: eleven cases at two runs each, and one judge run per
// case. A started eval passes this long after it starts.
const PLAN = { cases: 11, case_runs: 22, judge_runs: 11 };
const PERCENT_PER_RUN = 0.1;
const MINUTES_PER_RUN = 0.5;
const FLOOR = 5;
const EVAL_MS = 45_000;
const tenth = (value: number) => Math.round(value * 10) / 10;

let state: State | null = null;

const minutes = (now: number, delta: number) => new Date(now + delta * 60_000).toISOString();

// A 40-digit hex id that looks like a git object id and differs per `n`.
function hex(n: number): string {
  return Array.from({ length: 5 }, (_, i) =>
    (Math.imul(n + 1, 2654435761 + i * 40503) >>> 0).toString(16).padStart(8, "0"),
  ).join("");
}

function next(s: State, prefix: string): string {
  s.counter += 1;
  return `${prefix}${String(s.counter).padStart(26 - prefix.length, "0")}`;
}

// Where a seeded line came from; `record` fills in the revision.
const carried = (at: string): MemoryProvenance => ({
  revision_id: "",
  card: null,
  note: "carried over",
  at,
});
const byCard = (card: string, at: string): MemoryProvenance => ({
  revision_id: "",
  card,
  note: null,
  at,
});
const you = (at: string): MemoryProvenance => ({ revision_id: "", card: null, note: "you", at });

// Newest first.
function newestFirst<T>(items: T[], at: (item: T) => string): T[] {
  items.sort((a, b) => at(b).localeCompare(at(a)));
  return items;
}

// A holdout run of the current members that passed every case.
function passedRun(run: string, set: string, at: string): MemoryHoldout {
  return {
    eval_run: run,
    change_set: set,
    cases_passed: PLAN.cases,
    cases_total: PLAN.cases,
    passed: true,
    partial: false,
    current: true,
    ran_at: at,
  };
}

// A passing holdout run unless `failing` names the step that stops it. A
// failing one waits for its first holdout run, which the board can start.
function suggestion(
  s: State,
  op: MemorySuggestion["op"],
  fields: Partial<MemorySuggestion> & Pick<MemorySuggestion, "by" | "at">,
  failing?: { reason: string; hint: string },
): MemorySuggestion {
  const id = next(s, "01JPROP");
  const set = next(s, "01JCSET");
  const run = failing ? null : next(s, "01JEVAL");
  return {
    proposal_id: id,
    op,
    text: null,
    card: null,
    why: null,
    token_budget_after: null,
    target: null,
    gate: {
      proposal: id,
      ok: !failing,
      reason: failing?.reason ?? null,
      hint: failing?.hint ?? null,
      change_set: set,
      eval_run: run,
      measuring: null,
    },
    moved: false,
    moved_at: null,
    eval_clears: Boolean(failing),
    holdout: run ? passedRun(run, set, minutes(Date.parse(fields.at), 9)) : null,
    ...fields,
  };
}

// The text it was proposed against changed: Keep waits for a re-proposal.
function moved(sug: MemorySuggestion, at: string): MemorySuggestion {
  return {
    ...sug,
    moved: true,
    moved_at: at,
    eval_clears: false,
    gate: {
      ...sug.gate,
      ok: false,
      reason: "the text moved; re-propose it on the current text and run the holdout again",
      hint: "Re-propose on current text in the memory view, then run the holdout eval on the new proposal",
    },
  };
}

// A suggestion in no change set yet: a board eval makes its one-member set.
function unset(sug: MemorySuggestion): MemorySuggestion {
  return {
    ...sug,
    eval_clears: true,
    holdout: null,
    gate: {
      ...sug.gate,
      ok: false,
      reason: "the proposal is in no change set",
      hint: `tiller learn eval --proposal ${sug.proposal_id}`,
      change_set: null,
      eval_run: null,
    },
  };
}

const short = (id: string) => id.slice(-7);

// A history row with the fields a seeded or landed change shares.
function change(
  fields: Pick<MemoryChange, "proposal_id" | "kind" | "text" | "by" | "section"> &
    Partial<Omit<MemoryChange, "at">>,
): Omit<MemoryChange, "at"> {
  return {
    card: null,
    old_text: null,
    undone_at: null,
    from: fields.card ?? fields.by,
    ...fields,
  };
}

function seed(now: number): State {
  const s: State = {
    docs: [],
    landed: [],
    revisions: new Map(),
    jobs: [],
    sets: new Map(),
    counter: 0,
  };
  const line = (text: string, provenance: MemoryProvenance): Line => ({
    text,
    memory_id: next(s, "01JMEM"),
    provenance,
    suggestion: null,
  });
  const imported = minutes(now, -60 * 24 * 9);
  const sunday = minutes(now, -60 * 24 * 3);

  const fleetQuota = line(
    "Stop new worker dispatch when the weekly quota reaches 3%.",
    carried(imported),
  );
  fleetQuota.suggestion = suggestion(s, "rewrite", {
    text: "Stop starting new small-model workers at 3% of the week left, and new large-model workers at 8%.",
    by: "the curator",
    at: minutes(now, -34),
    why: "Large-model workers used about 3x the quota per card this week, and two were stopped mid-run.",
    token_budget_after: { orchestrator: 5_940, worker: 4_210 },
  });
  const fleet: Doc = {
    project_id: FLEET,
    name: "Fleet",
    path: "fleet.md",
    blob: "",
    dirty: false,
    sections: [
      {
        title: "How we work here",
        class: "rule",
        lines: [
          line(
            "Before asking me to merge a sizeable PR, or one that touches secrets, offer an independent review.",
            carried(imported),
          ),
          fleetQuota,
        ],
        suggestions: [],
      },
      {
        title: "Watch out for",
        class: "note",
        lines: [
          line(
            "Checks that can draw on screen, like the real-cursor e2e, run off screen or in a scratch space.",
            byCard("alpha-38", sunday),
          ),
        ],
        suggestions: [
          offScreen(s, {
            text: "If a project's pipeline cannot run its checks off screen, stop and ask me before running them.",
            by: "the curator",
            card: "alpha-38",
            at: minutes(now, -21),
            why: "The alpha test step opened the real-cursor e2e on screen even though the brief said off screen.",
            token_budget_after: { orchestrator: 5_980, worker: 4_250 },
          }),
        ],
      },
      {
        title: "Preferences",
        class: "preference",
        lines: [
          line(
            "Show every date and time in Lisbon time, and say which weekday it is.",
            you(imported),
          ),
          line(
            "Keep PR descriptions short: what it does for me first, then what I need to do.",
            you(imported),
          ),
        ],
        suggestions: [],
      },
    ],
  };

  const alpha: Doc = {
    project_id: "alpha",
    name: "alpha",
    path: "projects/alpha.md",
    blob: "",
    dirty: false,
    sections: [
      {
        title: "How we work here",
        class: "rule",
        lines: [
          line(
            "The remote hook must read stdin even when it queues nothing, or the agent's turn fails with a broken pipe.",
            byCard("alpha-38", sunday),
          ),
          line(
            "Logo explorations run as five parallel scouts. The pick comes back to me as one decision, with a contact sheet at 16, 32 and 128 px.",
            byCard("alpha-43", sunday),
          ),
        ],
        suggestions: [],
      },
      {
        title: "Watch out for",
        class: "note",
        lines: [
          line(
            "Wait for a week of debug logs before judging the click fallbacks. One day of data is not enough.",
            byCard("alpha-41", minutes(now, -60 * 24 * 8)),
          ),
        ],
        suggestions: [
          suggestion(s, "add", {
            text: "Check a few frames early in a launch video render; a full render takes over an hour.",
            by: "the alpha orchestrator",
            card: "alpha-42",
            at: minutes(now, -38),
            why: "The first frame check on alpha-42 only happened after about 50 minutes of rendering.",
            token_budget_after: { orchestrator: 6_120, worker: 4_980 },
          }),
        ],
      },
      {
        title: "Preferences",
        class: "preference",
        lines: [
          line(
            "Short reports: one contact sheet and three sentences, not a long write-up.",
            you(sunday),
          ),
        ],
        suggestions: [],
      },
    ],
  };

  const stale = line(
    "Cap beta at 3 concurrent workers. Parallel Rust builds overload the build host.",
    carried(imported),
  );
  stale.suggestion = moved(
    suggestion(s, "retire", {
      by: "the beta orchestrator",
      card: "beta-27",
      at: minutes(now, -60 * 5),
      why: "The project's capacity setting now caps workers, so this line repeats it.",
    }),
    minutes(now, -60 * 3),
  );
  // Its last board eval ended with nothing recorded.
  const flaky = unset(
    suggestion(s, "add", {
      text: "Run the slow suite with one worker when a test names a temp directory.",
      by: "the curator",
      card: "beta-28",
      at: minutes(now, -60 * 2),
      why: "Two builders hit the same flaky temp-directory test.",
    }),
  );
  s.jobs.push({
    view: {
      proposal_id: flaky.proposal_id,
      change_set: short(flaky.proposal_id),
      cases: PLAN.cases,
      started_at: minutes(now, -70),
      minutes: tenth(PLAN.case_runs * MINUTES_PER_RUN),
      error:
        "/home/user/tiller/evals/run.sh exited with code 1 and left no sealed/result.json; nothing recorded",
      finished_at: minutes(now, -64),
    },
    doneAt: 0,
  });
  const beta: Doc = {
    project_id: "beta",
    name: "beta",
    path: "projects/beta.md",
    blob: "",
    dirty: false,
    sections: [
      {
        title: "How we work here",
        class: "rule",
        lines: [
          stale,
          line(
            "Batch palette_set calls and keep a 1 h prompt cache to hold assistant cost down.",
            byCard("beta-27", sunday),
          ),
        ],
        suggestions: [],
      },
      {
        title: "Watch out for",
        class: "note",
        lines: [
          line(
            "Undo changes need a test for a cancelled stroke (Esc mid-drag).",
            byCard("beta-28", minutes(now, -60 * 2)),
          ),
        ],
        suggestions: [flaky],
      },
      { title: "Preferences", class: "preference", lines: [], suggestions: [] },
    ],
  };

  // Edited outside Tiller and not committed: every change is refused until
  // the file is committed or reset.
  const gamma: Doc = {
    project_id: "gamma",
    name: "gamma",
    path: "projects/gamma.md",
    blob: "",
    dirty: true,
    sections: [
      {
        title: "How we work here",
        class: "rule",
        lines: [
          line(
            "gamma has no PR CI by design. Report done once no CI is configured; do not wait on CI.",
            byCard("gamma-19", sunday),
          ),
        ],
        suggestions: [],
      },
      { title: "Watch out for", class: "note", lines: [], suggestions: [] },
      { title: "Preferences", class: "preference", lines: [], suggestions: [] },
    ],
  };

  s.docs = [fleet, alpha, beta, gamma];
  for (const doc of s.docs) {
    const revision = record(s, doc, "legacy-import", minutes(now, -60 * 24 * 9), null, null);
    for (const section of doc.sections) {
      for (const each of section.lines) each.provenance.revision_id = revision;
    }
  }

  const keptAt = minutes(now, -60 * 26);
  const kept = alpha.sections[0]?.lines[1];
  if (kept) {
    // The line shows the day it was kept, as its history row does.
    kept.provenance.at = keptAt;
    s.landed.push({
      project_id: "alpha",
      change: {
        ...change({
          proposal_id: next(s, "01JPROP"),
          kind: "kept",
          text: kept.text,
          by: "the alpha orchestrator",
          card: "alpha-43",
          section: "How we work here",
        }),
        at: keptAt,
      },
    });
  }
  s.landed.push(
    {
      project_id: "alpha",
      change: {
        ...change({
          proposal_id: next(s, "01JPROP"),
          kind: "edit",
          text: "Short reports: one contact sheet and three sentences, not a long write-up.",
          old_text: "Short reports: one contact sheet, not a long write-up.",
          by: "you",
          section: "Preferences",
        }),
        at: sunday,
      },
    },
    {
      project_id: "alpha",
      change: {
        ...change({
          proposal_id: next(s, "01JPROP"),
          kind: "kept",
          text: "Pin the bucket region in tests.",
          by: "the curator",
          card: "alpha-36",
          section: "Watch out for",
          undone_at: minutes(now, -60 * 24 * 4),
        }),
        at: minutes(now, -60 * 24 * 6),
      },
    },
  );
  const discarded = suggestion(s, "add", {
    text: "Never push to main without asking me.",
    by: "the beta orchestrator",
    card: "beta-27",
    at: minutes(now, -60 * 31),
  });
  s.landed.push({
    project_id: FLEET,
    change: {
      ...change({
        proposal_id: discarded.proposal_id,
        kind: "discard",
        text: discarded.text,
        by: discarded.by,
        card: discarded.card,
        section: "How we work here",
      }),
      at: minutes(now, -60 * 30),
    },
    discarded,
  });
  return s;
}

// The fleet suggestion that waits for its first holdout run, in a named set.
function offScreen(
  s: State,
  fields: Partial<MemorySuggestion> & Pick<MemorySuggestion, "by" | "at">,
): MemorySuggestion {
  const sug = suggestion(s, "add", fields, {
    reason: "change set off-screen checks has no holdout run",
    hint: "tiller learn eval off-screen checks",
  });
  s.sets.set(sug.proposal_id, "off-screen checks");
  return sug;
}

// A stand-in eval that has run its time passes, and Keep turns on.
function settle(s: State, now: number) {
  for (const job of s.jobs.filter((each) => each.view.finished_at === null)) {
    if (now < job.doneAt) continue;
    s.jobs.splice(s.jobs.indexOf(job), 1);
    const found = find(s, job.view.proposal_id);
    const sug = found && (found.line?.suggestion ?? found.section.suggestions[found.added]);
    if (!sug) continue;
    const run = next(s, "01JEVAL");
    const set = sug.gate.change_set ?? next(s, "01JCSET");
    const passed: MemorySuggestion = {
      ...sug,
      eval_clears: false,
      gate: { ...sug.gate, ok: true, reason: null, hint: null, change_set: set, eval_run: run },
      holdout: passedRun(run, set, new Date(now).toISOString()),
    };
    if (found.line) found.line.suggestion = passed;
    else found.section.suggestions[found.added] = passed;
  }
}

function current(): State {
  state ??= seed(Date.now());
  settle(state, Date.now());
  return state;
}

// Tests start each case from the seeded documents.
export function resetMemoryFixture(now: number = Date.now()): void {
  state = seed(now);
}

function fileText(doc: Doc): string {
  return doc.sections
    .map((section) =>
      [`## ${section.title}`, "", ...section.lines.map((l) => `- ${l.text}`)].join("\n"),
    )
    .join("\n\n")
    .concat("\n");
}

// A new blob and the revision row for it.
function record(
  s: State,
  doc: Doc,
  provenance: string,
  at: string,
  proposalId: string | null,
  card: string | null,
): string {
  s.counter += 1;
  doc.blob = hex(s.counter);
  const id = next(s, "01JREV");
  s.revisions.set(id, {
    id,
    project_id: doc.project_id,
    path: doc.path,
    blob_sha: doc.blob,
    commit_sha: hex(s.counter + 0x1000),
    review: "approved",
    provenance,
    recorded_at: at,
    approval: proposalId
      ? { id: next(s, "01JAPPR"), state: "succeeded", decided_by: "owner" }
      : null,
    proposal_id: proposalId,
    card,
    sessions:
      provenance === "legacy-import"
        ? [
            {
              session_id: next(s, "01JSESS"),
              role: "orchestrator",
              project_id: doc.project_id === FLEET ? "alpha" : doc.project_id,
              run_id: null,
              card: null,
              trigger: "startup",
              loaded_at: minutes(Date.parse(at), 60 * 24),
            },
          ]
        : [],
    text: fileText(doc),
  });
  return id;
}

function summary(doc: Doc): MemoryDocSummary {
  return {
    project_id: doc.project_id,
    name: doc.name,
    lines: doc.sections.reduce((n, section) => n + section.lines.length, 0),
    suggestions: doc.sections.reduce(
      (n, section) =>
        n + section.suggestions.length + section.lines.filter((l) => l.suggestion).length,
      0,
    ),
  };
}

function view(doc: Doc, landed: Landed[]): MemoryDocument {
  return {
    project_id: doc.project_id,
    name: doc.name,
    path: doc.path,
    blob: doc.blob,
    dirty: doc.dirty,
    sections: doc.sections.map((section) => ({
      title: section.title,
      class: section.class,
      lines: section.lines.map((l, index): MemoryLine => ({
        index,
        text: l.text,
        memory_id: l.memory_id,
        provenance: l.provenance,
        suggestion: l.suggestion && { ...l.suggestion, target: l.text },
      })),
      suggestions: section.suggestions,
    })),
    // The last 30 days, at most 50 rows, as the daemon sends them.
    changes: newestFirst(
      landed
        .filter((entry) => entry.project_id === doc.project_id)
        .map((entry) => entry.change)
        .filter((each) => Date.parse(each.at) >= Date.now() - 30 * 24 * 60 * 60_000),
      (each) => each.at,
    ).slice(0, 50),
  };
}

// What a holdout eval of `proposalId` will cost against the weekly window
// with the least left, and why it would not start, as the daemon plans it.
function plan(
  s: State,
  proposalId: string,
  windows: QuotaWindow[],
  now: number,
): MemoryEvalPlan | undefined {
  const found = find(s, proposalId);
  const sug = found && (found.line?.suggestion ?? found.section.suggestions[found.added]);
  if (!sug) return undefined;
  const refusals: string[] = [];
  const busy = s.jobs.find((job) => job.view.finished_at === null);
  if (busy) {
    refusals.push(`a holdout eval is already running, on change set ${busy.view.change_set}`);
  }
  if (sug.moved) {
    refusals.push(
      "the document changed after this was proposed; re-propose it on the current text first",
    );
  } else if (!sug.eval_clears) {
    refusals.push(
      sug.holdout?.current
        ? `the holdout already ran on these members: ${sug.holdout.cases_passed} of ${sug.holdout.cases_total} cases`
        : (sug.gate.reason ?? "the gate already passes"),
    );
  }
  const modelRuns = PLAN.case_runs + PLAN.judge_runs;
  const percent = tenth(modelRuns * PERCENT_PER_RUN);
  const weekly = windows.filter((each) => each.kind === "weekly");
  // The weekly window with the least left binds, as the daemon checks it.
  const least = weekly.reduce<QuotaWindow | undefined>(
    (min, each) =>
      !min || (each.percent_remaining ?? 0) < (min.percent_remaining ?? 0) ? each : min,
    undefined,
  );
  const left = least?.percent_remaining ?? null;
  // A run cannot leave less than nothing.
  const after = left === null ? null : Math.max(0, tenth(left - percent));
  if (!least) {
    refusals.push("there is no weekly quota reading, and the quota floor fails closed");
  } else if (weekly.some((each) => each.stale)) {
    refusals.push("a weekly quota reading is stale, and the quota floor fails closed");
  } else if (after === null) {
    refusals.push("a weekly quota window has no reading, and the quota floor fails closed");
  } else if (after <= FLOOR) {
    refusals.push(
      `it would leave ${after} percent of the week, at or under the ${FLOOR} percent floor`,
    );
  }
  // The stand-in windows reset on a fixed day; the plan names the next one.
  const resets = least?.resets_at ?? null;
  return {
    proposal_id: sug.proposal_id,
    change_set: s.sets.get(sug.proposal_id) ?? short(sug.proposal_id),
    ...PLAN,
    model_runs: modelRuns,
    percent,
    minutes: tenth(PLAN.case_runs * MINUTES_PER_RUN),
    quota: {
      label: least?.label ?? null,
      left,
      floor: FLOOR,
      left_after: after,
      resets_at:
        resets && Date.parse(resets) > now ? resets : least ? minutes(now, 60 * 24 * 4) : null,
      fetched_at: least?.fetched_at ?? null,
    },
    refusal: refusals[0] ?? null,
  };
}

// `GET /api/v1/memory?scope=`, `GET /api/v1/memory/revisions/{id}` and
// `GET /api/v1/memory/proposals/{id}/eval`, which plans against `quota`;
// `undefined` is a 404.
export function memoryResource(
  url: URL,
  quota: QuotaWindow[] = [],
  now: number = Date.now(),
): MemoryResponse | MemoryRevision | MemoryEvalPlan | undefined {
  const s = current();
  if (url.pathname === "/api/v1/memory") {
    const scope = url.searchParams.get("scope") ?? FLEET;
    const doc = s.docs.find((each) => each.project_id === scope);
    if (!doc) return undefined;
    return {
      docs: s.docs.map(summary),
      doc: view(doc, s.landed),
      evals: newestFirst(
        s.jobs.map((job) => job.view),
        (job) => job.started_at,
      ),
    };
  }
  const revision = /^\/api\/v1\/memory\/revisions\/([^/]+)$/.exec(url.pathname);
  if (revision?.[1]) return s.revisions.get(revision[1]);
  const evalPlan = /^\/api\/v1\/memory\/proposals\/([^/]+)\/eval$/.exec(url.pathname);
  if (evalPlan?.[1]) return plan(s, evalPlan[1], quota, now);
  return undefined;
}

export type MemoryReply =
  | { ok: true; data: unknown; approvalId: string | null }
  | { ok: false; status: number; code: string; message: string; current?: object };

const refuse = (message: string, latest?: object): MemoryReply => ({
  ok: false,
  status: 422,
  code: "refused",
  message,
  current: latest,
});

function writable(doc: Doc | undefined, base: unknown): MemoryReply | Doc {
  if (!doc)
    return { ok: false, status: 404, code: "not_found", message: "no such memory document" };
  if (doc.dirty) {
    return refuse(
      `${doc.path} has changes that are not committed; commit or reset it, then try again`,
    );
  }
  if (base !== doc.blob) {
    return refuse(`${doc.path} changed since you opened it; here is the current text`, {
      blob: doc.blob,
    });
  }
  return doc;
}

function land(s: State, doc: Doc, row: Omit<MemoryChange, "at">, card: string | null): MemoryReply {
  const at = new Date().toISOString();
  const revision = record(s, doc, "tiller", at, row.proposal_id, card);
  for (const section of doc.sections) {
    for (const each of section.lines)
      if (each.provenance.revision_id === "") {
        each.provenance.revision_id = revision;
      }
  }
  s.landed.push({ project_id: doc.project_id, change: { ...row, at } });
  const approvalId = next(s, "01JAPPR");
  const landed: MemoryLanded = {
    proposal_id: row.proposal_id,
    approval_id: approvalId,
    blob: doc.blob,
  };
  return { ok: true, data: landed, approvalId };
}

function edit(s: State, body: Record<string, unknown>): MemoryReply {
  const doc = writable(
    s.docs.find((each) => each.project_id === body.project),
    body.base_blob,
  );
  if (!("sections" in doc)) return doc;
  const section = doc.sections.find((each) => each.title === body.section);
  if (!section) return refuse(`no section ${String(body.section)} in ${doc.path}`);
  const text = typeof body.text === "string" ? body.text.replace(/\s+/g, " ").trim() : "";
  const index = typeof body.index === "number" ? body.index : -1;
  const proposalId = next(s, "01JPROP");
  switch (body.op) {
    case "add":
      if (!text) return refuse("a line needs text");
      section.lines.push({
        text,
        memory_id: next(s, "01JMEM"),
        provenance: you(new Date().toISOString()),
        suggestion: null,
      });
      return land(
        s,
        doc,
        change({ proposal_id: proposalId, kind: "added", text, by: "you", section: section.title }),
        null,
      );
    case "rewrite": {
      const target = section.lines[index];
      if (!target) return refuse(`no line ${index} in ${section.title}`);
      if (!text) return refuse("a line needs text; remove it instead");
      const old = target.text;
      target.text = text;
      target.provenance = you(new Date().toISOString());
      return land(
        s,
        doc,
        change({
          proposal_id: proposalId,
          kind: "edit",
          text,
          old_text: old,
          by: "you",
          section: section.title,
        }),
        null,
      );
    }
    case "remove": {
      const target = section.lines[index];
      if (!target) return refuse(`no line ${index} in ${section.title}`);
      section.lines.splice(index, 1);
      return land(
        s,
        doc,
        change({
          proposal_id: proposalId,
          kind: "remove",
          text: target.text,
          by: "you",
          section: section.title,
        }),
        null,
      );
    }
    default:
      return refuse("op must be add, rewrite or remove");
  }
}

function find(s: State, proposalId: string) {
  for (const doc of s.docs) {
    for (const section of doc.sections) {
      const added = section.suggestions.findIndex((each) => each.proposal_id === proposalId);
      if (added !== -1) return { doc, section, added, line: null };
      const line = section.lines.find((each) => each.suggestion?.proposal_id === proposalId);
      if (line) return { doc, section, added: -1, line };
    }
  }
  return null;
}

function decideOne(s: State, decision: Record<string, unknown>): MemoryReply {
  const proposalId = String(decision.proposal);
  const found = find(s, proposalId);
  if (!found) return refuse(`proposal ${proposalId} is not open`);
  const doc = writable(found.doc, decision.base_blob);
  if (!("sections" in doc)) return doc;
  const { section, added, line } = found;
  const sug = line?.suggestion ?? section.suggestions[added];
  if (!sug) return refuse(`proposal ${proposalId} is not open`);
  const row = (kind: MemoryChange["kind"], text: string | null, old: string | null = null) =>
    change({
      proposal_id: proposalId,
      kind,
      text,
      old_text: old,
      by: sug.by,
      card: sug.card,
      section: section.title,
    });
  if (decision.decision === "discard") {
    if (line) line.suggestion = null;
    else section.suggestions.splice(added, 1);
    s.landed.push({
      project_id: doc.project_id,
      change: { ...row("discard", sug.text ?? line?.text ?? null), at: new Date().toISOString() },
      discarded: sug,
    });
    return {
      ok: true,
      data: { proposal_id: proposalId, approval_id: null, blob: doc.blob },
      approvalId: null,
    };
  }
  if (decision.decision !== "keep") return refuse("decision must be keep or discard");
  if (!sug.gate.ok) {
    return refuse(`proposal ${proposalId}: ${sug.gate.reason}; ${sug.gate.hint}`);
  }
  const provenance: MemoryProvenance = {
    revision_id: "",
    card: sug.card,
    note: sug.card ? null : sug.by,
    at: new Date().toISOString(),
  };
  if (sug.op === "add") {
    section.suggestions.splice(added, 1);
    section.lines.push({
      text: sug.text ?? "",
      memory_id: next(s, "01JMEM"),
      provenance,
      suggestion: null,
    });
    return land(s, doc, row("kept", sug.text), sug.card);
  }
  if (!line) return refuse(`the line proposal ${proposalId} changes is gone`);
  if (sug.op === "rewrite") {
    const old = line.text;
    line.text = sug.text ?? line.text;
    line.provenance = provenance;
    line.suggestion = null;
    return land(s, doc, row("edit", line.text, old), sug.card);
  }
  section.lines.splice(section.lines.indexOf(line), 1);
  return land(s, doc, row("remove", line.text), sug.card);
}

function decide(s: State, body: Record<string, unknown>): MemoryReply {
  const decisions = Array.isArray(body.decisions)
    ? (body.decisions as Record<string, unknown>[])
    : [];
  if (decisions.length === 0) return refuse("decide needs at least one decision");
  const out: unknown[] = [];
  let approvalId: string | null = null;
  for (const each of decisions) {
    const reply = decideOne(s, each);
    if (!reply.ok) return reply;
    out.push(reply.data);
    approvalId ??= reply.approvalId;
  }
  return { ok: true, data: { decisions: out }, approvalId };
}

// Starts the stand-in holdout eval, which passes EVAL_MS later.
function startEval(s: State, proposalId: string, quota: QuotaWindow[]): MemoryReply {
  const now = Date.now();
  const planned = plan(s, proposalId, quota, now);
  if (!planned) return { ok: false, status: 404, code: "not_found", message: "no such proposal" };
  if (planned.refusal) return refuse(`proposal ${proposalId}: ${planned.refusal}`);
  // A later start replaces the failed one it retries.
  s.jobs = s.jobs.filter((job) => job.view.proposal_id !== proposalId);
  const job: MemoryEvalJob = {
    proposal_id: proposalId,
    change_set: planned.change_set,
    cases: planned.cases,
    started_at: new Date(now).toISOString(),
    minutes: planned.minutes,
    error: null,
    finished_at: null,
  };
  s.jobs.push({ view: job, doneAt: now + EVAL_MS });
  return { ok: true, data: job, approvalId: null };
}

// The same change on the current text, as a new proposal in no change set.
function repropose(s: State, proposalId: string, body: Record<string, unknown>): MemoryReply {
  const found = find(s, proposalId);
  const sug = found && (found.line?.suggestion ?? found.section.suggestions[found.added]);
  if (!found || !sug) return refuse(`proposal ${proposalId}: it is discarded, not proposed`);
  const doc = writable(found.doc, body.base_blob);
  if (!("sections" in doc)) return doc;
  if (!sug.moved) {
    return refuse(
      `proposal ${proposalId}: it was made on the current text; there is nothing to re-propose`,
    );
  }
  const id = next(s, "01JPROP");
  const fresh = unset({
    ...sug,
    proposal_id: id,
    moved: false,
    moved_at: null,
    at: new Date().toISOString(),
    gate: { ...sug.gate, proposal: id },
  });
  if (found.line) found.line.suggestion = fresh;
  else found.section.suggestions[found.added] = fresh;
  s.jobs = s.jobs.filter((job) => job.view.proposal_id !== proposalId);
  return {
    ok: true,
    data: { proposal_id: id, approval_id: null, blob: doc.blob },
    approvalId: null,
  };
}

// Reverses a change in the history, or proposes a discard again.
function undo(s: State, proposalId: string, body: Record<string, unknown>): MemoryReply {
  const entry = s.landed.find((each) => each.change.proposal_id === proposalId);
  if (!entry) return { ok: false, status: 404, code: "not_found", message: "no such proposal" };
  const doc = writable(
    s.docs.find((each) => each.project_id === entry.project_id),
    body.base_blob,
  );
  if (!("sections" in doc)) return doc;
  const { change: row } = entry;
  if (row.undone_at) return refuse(`proposal ${proposalId}: it was already undone`);
  const section = doc.sections.find((each) => each.title === row.section);
  if (!section) return refuse(`proposal ${proposalId}: no section ${row.section} in ${doc.path}`);
  const at = new Date().toISOString();
  const gone = refuse(`proposal ${proposalId}: the line it changed is no longer in the file`);
  if (row.kind === "discard") {
    const again = entry.discarded;
    if (!again) return refuse(`proposal ${proposalId}: it cannot be proposed again`);
    const id = next(s, "01JPROP");
    const fresh = unset({ ...again, proposal_id: id, at, gate: { ...again.gate, proposal: id } });
    if (again.op === "add") {
      section.suggestions.push(fresh);
    } else {
      const target = section.lines.find((each) => each.text === again.target);
      if (!target) return gone;
      target.suggestion = fresh;
    }
    row.undone_at = at;
    return {
      ok: true,
      data: { proposal_id: id, approval_id: null, blob: doc.blob },
      approvalId: null,
    };
  }
  const at_line = section.lines.findIndex((each) => each.text === row.text);
  if (row.kind === "remove") {
    section.lines.push({
      text: row.text ?? "",
      memory_id: next(s, "01JMEM"),
      provenance: you(at),
      suggestion: null,
    });
  } else if (at_line === -1) {
    return gone;
  } else if (row.kind === "edit") {
    const target = section.lines[at_line];
    if (!target || row.old_text === null) return gone;
    target.text = row.old_text;
    target.provenance = you(at);
  } else {
    section.lines.splice(at_line, 1);
  }
  row.undone_at = at;
  const id = next(s, "01JPROP");
  record(s, doc, "tiller", at, id, null);
  return {
    ok: true,
    data: { proposal_id: id, approval_id: null, blob: doc.blob },
    approvalId: null,
  };
}

// A memory mutation, or `null` when `path` is not one. A holdout eval plans
// against `quota`.
export function memoryMutation(
  method: string,
  path: string,
  body: Record<string, unknown>,
  quota: QuotaWindow[] = [],
): MemoryReply | null {
  if (method !== "POST") return null;
  const s = current();
  if (path === "/api/v1/memory/edits") return edit(s, body);
  if (path === "/api/v1/memory/proposals/decide") return decide(s, body);
  const proposal = /^\/api\/v1\/memory\/proposals\/([^/]+)\/(eval|repropose)$/.exec(path);
  if (proposal?.[1] && proposal[2] === "eval") return startEval(s, proposal[1], quota);
  if (proposal?.[1]) return repropose(s, proposal[1], body);
  const reverse = /^\/api\/v1\/memory\/changes\/([^/]+)\/undo$/.exec(path);
  if (reverse?.[1]) return undo(s, reverse[1], body);
  return null;
}
