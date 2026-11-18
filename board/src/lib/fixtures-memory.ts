// The memory view for the dev and preview servers and for tests: a stand-in
// kb store and the pending queue, kept in memory so Edit, Keep and Discard
// change what the next read answers (memory.md section 4). Titles and bodies
// are placeholders. The `memory-limits` scenario has no kb on the host, and
// `empty` has a kb with no notes.
import type {
  KbState,
  KeptNote,
  LiveNote,
  MemoryKind,
  MemoryResponse,
  PendingNote,
} from "./api-types.ts";

// One note in the stand-in kb store.
interface KbNote {
  id: string;
  title: string;
  kind: string;
  scope: string;
  tokens: number;
  // A file the loader could not parse: skipped as `unreadable`.
  broken?: boolean;
}

interface State {
  scenario: string;
  kb: KbState;
  notes: KbNote[];
  pending: PendingNote[];
  counter: number;
}

const KINDS: readonly MemoryKind[] = ["convention", "decision", "gotcha", "how-to"];
const GLOBAL = "global";
const LIMIT = 7500;
const KB_HOME = "/home/user/.kb";

// The role file's own tokens, which the budget counts first.
const ROLE_TOKENS: Record<string, number> = {
  builder: 2140,
  reviewer: 1720,
  verifier: 1460,
  scout: 1380,
  orchestrator: 2960,
  curator: 1280,
};

// A Keep whose body matches this is refused by the stand-in kb.
const KB_REFUSES = /\btoken\b/i;

let state: State | null = null;

const minutes = (now: number, delta: number) => new Date(now + delta * 60_000).toISOString();

// The project's repo scope, as kb names it.
const repoScope = (project: string) => `github.com/example/${project}`;

const cardId = (n: number) => `01JCARD${String(n).padStart(19, "0")}`;
const runId = (n: number) => `01JRUN${String(n).padStart(20, "0")}`;

function pending(
  fields: Omit<PendingNote, "edited_at" | "rev"> & Partial<PendingNote>,
): PendingNote {
  return { edited_at: null, rev: 1, ...fields };
}

function seedNotes(): KbNote[] {
  const alpha = repoScope("alpha");
  const beta = repoScope("beta");
  return [
    {
      id: "actions-sit-on-the-right",
      title: "Actions sit on the right of their card, the primary one last",
      kind: "convention",
      scope: alpha,
      tokens: 180,
    },
    {
      id: "the-product-name-is-lowercase",
      title: "The product name tiller is written lowercase",
      kind: "convention",
      scope: GLOBAL,
      tokens: 60,
    },
    {
      id: "exports-run-on-one-worker",
      title: "Exports run on one worker until the queue is split",
      kind: "decision",
      scope: alpha,
      tokens: 240,
    },
    {
      id: "jsdom-needs-a-resize-observer",
      title: "Components that measure themselves need a ResizeObserver stub in jsdom",
      kind: "gotcha",
      scope: alpha,
      tokens: 210,
    },
    {
      id: "the-slow-suite-needs-one-worker",
      title: "The slow suite fails when it runs on more than one worker",
      kind: "gotcha",
      scope: beta,
      tokens: 150,
    },
    {
      id: "full-export-runbook",
      title: "How to run a full export by hand, step by step",
      kind: "how-to",
      scope: alpha,
      tokens: 5200,
    },
    {
      id: "export-job-open-work",
      title: "Export job: the retry path is half done",
      kind: "open-work",
      scope: alpha,
      tokens: 140,
    },
    {
      id: "release-checklist",
      title: "How to cut a release",
      kind: "how-to",
      scope: GLOBAL,
      tokens: 420,
    },
    {
      id: "old-format",
      title: "old-format.md",
      kind: "convention",
      scope: GLOBAL,
      tokens: 0,
      broken: true,
    },
  ];
}

function seedPending(now: number): PendingNote[] {
  return [
    pending({
      id: "01JPEND0000000000000000001",
      project_id: "alpha",
      kind: "gotcha",
      title: "A new owner-only route also needs an authority test",
      body: "Else the authority test for the routes fails on the next PR. Add the route to the owner list in the same change.",
      source: "fleet",
      role: "builder",
      card_id: cardId(4),
      run_id: runId(41),
      retro_id: null,
      created_at: minutes(now, -125),
    }),
    pending({
      id: "01JPEND0000000000000000002",
      project_id: "beta",
      kind: "gotcha",
      title: "The bench forge stand-in has no checks command",
      body: "On the usability bench, wait on the card instead of the PR checks.",
      source: "curator",
      role: null,
      card_id: cardId(28),
      run_id: null,
      retro_id: "01JRETRO000000000000000028",
      created_at: minutes(now, -26 * 60 - 41),
    }),
    pending({
      id: "01JPEND0000000000000000003",
      project_id: "gamma",
      kind: "how-to",
      title: "Read the deploy token from the runner's store",
      body: "The deploy step reads its token from the runner's secret store, never from the repo.",
      source: "fleet",
      role: "reviewer",
      card_id: cardId(31),
      run_id: runId(311),
      retro_id: null,
      created_at: minutes(now, -50),
      edited_at: minutes(now, -20),
      rev: 2,
    }),
    pending({
      id: "01JPEND0000000000000000004",
      project_id: "alpha",
      kind: "convention",
      title: "Post a short summary before asking for a merge",
      body: "Before asking me to merge a sizeable PR, post a short summary of what changed and how it was tested.",
      source: "migration",
      role: null,
      card_id: null,
      run_id: null,
      retro_id: null,
      created_at: minutes(now, -3 * 24 * 60),
    }),
  ];
}

function seed(now: number, scenario: string): State {
  const missing = scenario === "memory-limits";
  return {
    scenario,
    kb: missing ? "missing" : "ok",
    notes: missing || scenario === "empty" ? [] : seedNotes(),
    pending: scenario === "empty" ? [] : seedPending(now),
    counter: 100,
  };
}

// The queue, seeded again when `scenario` differs from the one it has.
function current(scenario?: string): State {
  if (!state || (scenario !== undefined && scenario !== state.scenario)) {
    state = seed(Date.now(), scenario ?? state?.scenario ?? "default");
  }
  return state;
}

// Tests start each case from the seeded queue.
export function resetMemoryFixture(now: number = Date.now(), scenario = "default"): void {
  state = seed(now, scenario);
}

const KIND_ORDER = ["convention", "decision", "gotcha", "how-to"];
const scopeRank = (scope: string, repo: string) => (scope === repo ? 0 : 1);

// What a launch of `role` in `project` loads now, in load order, and the
// budget it uses (memory.md section 3). Open work is never loaded.
function live(s: State, project: string, role: string): { used: number; notes: LiveNote[] } {
  const repo = repoScope(project);
  const shown = s.notes.filter(
    (note) => (note.scope === repo || note.scope === GLOBAL) && note.kind !== "open-work",
  );
  shown.sort(
    (a, b) =>
      scopeRank(a.scope, repo) - scopeRank(b.scope, repo) ||
      KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) ||
      a.id.localeCompare(b.id),
  );
  let used = ROLE_TOKENS[role] ?? 0;
  const notes = shown.map((note): LiveNote => {
    const base = {
      id: note.id,
      title: note.title,
      kind: note.kind,
      scope: note.scope,
      tokens: note.tokens,
    };
    if (note.broken) return { ...base, loaded: false, reason: "unreadable" };
    if (used + note.tokens > LIMIT) return { ...base, loaded: false, reason: "over budget" };
    used += note.tokens;
    return { ...base, loaded: true, reason: null };
  });
  return { used, notes };
}

// `GET /api/v1/memory?project=&role=`, or `undefined` for any other path.
export function memoryResource(url: URL, scenario?: string): MemoryResponse | undefined {
  if (url.pathname !== "/api/v1/memory") return undefined;
  const s = current(scenario);
  const project = url.searchParams.get("project");
  const role = url.searchParams.get("role") ?? "builder";
  const loads = project && s.kb === "ok" ? live(s, project, role) : null;
  const queue = s.pending.filter((note) => !project || note.project_id === project);
  queue.sort((a, b) => b.created_at.localeCompare(a.created_at));
  return {
    project_id: project,
    role,
    kb: {
      state: s.kb,
      home: KB_HOME,
      scopes: project ? [repoScope(project), GLOBAL] : [GLOBAL],
    },
    budget: { used: loads?.used ?? ROLE_TOKENS[role] ?? 0, limit: LIMIT },
    live: loads?.notes ?? [],
    pending: queue,
  };
}

export type MemoryReply =
  | { ok: true; data: unknown; kind: string; txn: string }
  | { ok: false; status: number; code: string; message: string; current?: object };

function find(s: State, id: string, rev: unknown): PendingNote | MemoryReply {
  const note = s.pending.find((each) => each.id === id);
  if (!note) {
    return { ok: false, status: 404, code: "not_found", message: `no pending note ${id}` };
  }
  if (rev !== note.rev) {
    return {
      ok: false,
      status: 409,
      code: "conflict",
      message: "This note changed since you opened it. Cancel to see the new text.",
      current: { ...note },
    };
  }
  return note;
}

const invalid = (message: string): MemoryReply => ({
  ok: false,
  status: 422,
  code: "invalid",
  message,
});

const isText = (value: unknown, max: number) =>
  typeof value === "string" && value.trim().length >= 1 && value.length <= max;

function edit(s: State, id: string, body: Record<string, unknown>): MemoryReply {
  const found = find(s, id, body.rev);
  if ("ok" in found) return found;
  const { title, kind } = body;
  const text = body.body;
  if (title !== undefined && !isText(title, 120))
    return invalid("The title must be 1 to 120 characters.");
  if (text !== undefined && !isText(text, 2000))
    return invalid("The body must be 1 to 2000 characters.");
  if (kind !== undefined && !KINDS.includes(kind as MemoryKind))
    return invalid(`The kind must be one of ${KINDS.join(", ")}.`);
  if (typeof title === "string") found.title = title;
  if (typeof text === "string") found.body = text;
  if (kind !== undefined) found.kind = kind as MemoryKind;
  found.edited_at = new Date().toISOString();
  found.rev += 1;
  return { ok: true, data: { ...found }, kind: "memory.edited", txn: "mem.edit" };
}

function keep(s: State, id: string, body: Record<string, unknown>): MemoryReply {
  const found = find(s, id, body.rev);
  if ("ok" in found) return found;
  if (s.kb !== "ok") {
    return {
      ok: false,
      status: 422,
      code: "refused",
      message: "tiller cannot find the kb command: set memory.kb_command",
    };
  }
  if (KB_REFUSES.test(`${found.title} ${found.body}`)) {
    return {
      ok: false,
      status: 422,
      code: "refused",
      message:
        "kb add: the note looks like it holds a secret (it names a token). Reword it and try again.",
    };
  }
  const scope = repoScope(found.project_id);
  const noteId = found.title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 60);
  s.notes.push({
    id: noteId,
    title: found.title,
    kind: found.kind,
    scope,
    tokens: Math.ceil((found.title.length + found.body.length) / 4) + 12,
  });
  s.pending = s.pending.filter((each) => each.id !== id);
  const data: KeptNote = { note_id: noteId, scope };
  return { ok: true, data, kind: "memory.kept", txn: "mem.keep" };
}

function discard(s: State, id: string, body: Record<string, unknown>): MemoryReply {
  const found = find(s, id, body.rev);
  if ("ok" in found) return found;
  s.pending = s.pending.filter((each) => each.id !== id);
  return { ok: true, data: {}, kind: "memory.discarded", txn: "mem.discard" };
}

// A memory mutation, or `null` when `path` is not one.
export function memoryMutation(
  method: string,
  path: string,
  body: Record<string, unknown>,
  scenario?: string,
): MemoryReply | null {
  const found = /^\/api\/v1\/memory\/pending\/([^/]+)(?:\/(keep|discard))?$/.exec(path);
  const id = found?.[1];
  if (!id) return null;
  const s = current(scenario);
  const action = found[2];
  if (method === "PATCH" && action === undefined) return edit(s, id, body);
  if (method === "POST" && action === "keep") return keep(s, id, body);
  if (method === "POST" && action === "discard") return discard(s, id, body);
  return null;
}

// The one note a stand-in curator suggests when its retro ends (memory.md
// section 5). Answers its id.
export function curatorNote(
  project: string,
  card: string,
  retro: string,
  title: string,
  text: string,
  at: string,
): string {
  const s = current();
  s.counter += 1;
  const id = `01JPEND${String(s.counter).padStart(19, "0")}`;
  s.pending.push(
    pending({
      id,
      project_id: project,
      kind: "gotcha",
      title,
      body: text,
      source: "curator",
      role: null,
      card_id: card,
      run_id: null,
      retro_id: retro,
      created_at: at,
    }),
  );
  return id;
}
