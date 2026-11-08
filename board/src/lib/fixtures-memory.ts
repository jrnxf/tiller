// The memory view for the dev and preview servers and for tests: one
// document per project plus the shared one, kept in memory so Keep, Discard,
// and an edit change what the next read answers (memory.md section 7,
// protocol.md sections 7.3 and 7.4). Names and lines are placeholders.
import type {
  MemoryChange,
  MemoryDocSummary,
  MemoryDocument,
  MemoryLanded,
  MemoryLine,
  MemoryProvenance,
  MemoryResponse,
  MemoryRevision,
  MemorySuggestion,
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

// One landed change.
interface Landed {
  project_id: string;
  change: MemoryChange;
}

interface State {
  docs: Doc[];
  landed: Landed[];
  revisions: Map<string, MemoryRevision>;
  counter: number;
}

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

// A passing holdout run unless `failing` names the step that stops it.
function suggestion(
  s: State,
  op: MemorySuggestion["op"],
  fields: Partial<MemorySuggestion> & Pick<MemorySuggestion, "by" | "at">,
  failing?: { reason: string; hint: string },
): MemorySuggestion {
  const id = next(s, "01JPROP");
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
      change_set: next(s, "01JCSET"),
      eval_run: failing ? null : next(s, "01JEVAL"),
      measuring: null,
    },
    ...fields,
  };
}

function seed(now: number): State {
  const s: State = { docs: [], landed: [], revisions: new Map(), counter: 0 };
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
          suggestion(
            s,
            "add",
            {
              text: "If a project's pipeline cannot run its checks off screen, stop and ask me before running them.",
              by: "the curator",
              card: "alpha-38",
              at: minutes(now, -21),
              why: "The alpha test step opened the real-cursor e2e on screen even though the brief said off screen.",
              token_budget_after: { orchestrator: 5_980, worker: 4_250 },
            },
            {
              reason: "change set off-screen checks has no holdout run",
              hint: "tiller learn eval off-screen checks",
            },
          ),
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
  stale.suggestion = suggestion(
    s,
    "retire",
    {
      by: "the beta orchestrator",
      card: "beta-27",
      at: minutes(now, -60 * 5),
      why: "The project's capacity setting now caps workers, so this line repeats it.",
    },
    {
      reason: "the text moved; re-base and run the holdout again",
      hint: "discard it, undo the discard to propose it on the current text, then tiller learn eval --proposal <new>",
    },
  );
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
        suggestions: [],
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
    s.landed.push({
      project_id: "alpha",
      change: {
        proposal_id: next(s, "01JPROP"),
        kind: "kept",
        text: kept.text,
        from: "alpha-43",
        at: keptAt,
      },
    });
  }
  s.landed.push({
    project_id: FLEET,
    change: {
      proposal_id: next(s, "01JPROP"),
      kind: "discard",
      text: "Never push to main without asking me.",
      from: "beta-27",
      at: minutes(now, -60 * 30),
    },
  });
  return s;
}

function current(): State {
  state ??= seed(Date.now());
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
    changes: newestFirst(
      landed.filter((entry) => entry.project_id === doc.project_id).map((entry) => entry.change),
      (change) => change.at,
    ).slice(0, 5),
  };
}

// `GET /api/v1/memory?scope=` and `GET /api/v1/memory/revisions/{id}`;
// `undefined` is a 404.
export function memoryResource(url: URL): MemoryResponse | MemoryRevision | undefined {
  const s = current();
  if (url.pathname === "/api/v1/memory") {
    const scope = url.searchParams.get("scope") ?? FLEET;
    const doc = s.docs.find((each) => each.project_id === scope);
    if (!doc) return undefined;
    return { docs: s.docs.map(summary), doc: view(doc, s.landed) };
  }
  const revision = /^\/api\/v1\/memory\/revisions\/([^/]+)$/.exec(url.pathname);
  if (revision?.[1]) return s.revisions.get(revision[1]);
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

function land(
  s: State,
  doc: Doc,
  change: Omit<MemoryChange, "at">,
  card: string | null,
): MemoryReply {
  const at = new Date().toISOString();
  const revision = record(s, doc, "tiller", at, change.proposal_id, card);
  for (const section of doc.sections) {
    for (const each of section.lines)
      if (each.provenance.revision_id === "") {
        each.provenance.revision_id = revision;
      }
  }
  s.landed.push({ project_id: doc.project_id, change: { ...change, at } });
  const approvalId = next(s, "01JAPPR");
  const landed: MemoryLanded = {
    proposal_id: change.proposal_id,
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
      return land(s, doc, { proposal_id: proposalId, kind: "added", text, from: "you" }, null);
    case "rewrite": {
      const target = section.lines[index];
      if (!target) return refuse(`no line ${index} in ${section.title}`);
      if (!text) return refuse("a line needs text; remove it instead");
      target.text = text;
      target.provenance = you(new Date().toISOString());
      return land(s, doc, { proposal_id: proposalId, kind: "edit", text, from: "you" }, null);
    }
    case "remove": {
      const target = section.lines[index];
      if (!target) return refuse(`no line ${index} in ${section.title}`);
      section.lines.splice(index, 1);
      return land(
        s,
        doc,
        { proposal_id: proposalId, kind: "remove", text: target.text, from: "you" },
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
  const from = sug.card ?? sug.by;
  if (decision.decision === "discard") {
    if (line) line.suggestion = null;
    else section.suggestions.splice(added, 1);
    s.landed.push({
      project_id: doc.project_id,
      change: {
        proposal_id: proposalId,
        kind: "discard",
        text: sug.text ?? line?.text ?? null,
        from,
        at: new Date().toISOString(),
      },
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
    return land(s, doc, { proposal_id: proposalId, kind: "kept", text: sug.text, from }, sug.card);
  }
  if (!line) return refuse(`the line proposal ${proposalId} changes is gone`);
  if (sug.op === "rewrite") {
    line.text = sug.text ?? line.text;
    line.provenance = provenance;
    line.suggestion = null;
    return land(s, doc, { proposal_id: proposalId, kind: "edit", text: line.text, from }, sug.card);
  }
  section.lines.splice(section.lines.indexOf(line), 1);
  return land(s, doc, { proposal_id: proposalId, kind: "remove", text: line.text, from }, sug.card);
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

// A memory mutation, or `null` when `path` is not one.
export function memoryMutation(
  method: string,
  path: string,
  body: Record<string, unknown>,
): MemoryReply | null {
  if (method !== "POST") return null;
  const s = current();
  if (path === "/api/v1/memory/edits") return edit(s, body);
  if (path === "/api/v1/memory/proposals/decide") return decide(s, body);
  return null;
}
