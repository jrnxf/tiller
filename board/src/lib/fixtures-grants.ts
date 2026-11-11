// The grants of the dev and preview servers and of tests: alpha starts with
// two live grants and an ended one, kept in memory so New grant and Revoke
// change what the next read answers (authority.md section 6, protocol.md
// sections 7.3 and 7.4). The daemon's refusals are mirrored here.
import type { GrantConditions, GrantRow, ProjectGrants } from "./api-types.ts";
import { lisbonField16 } from "./clock.ts";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

// What a grant mutation answers: the row, or the daemon's refusal.
type GrantReply =
  | { ok: true; data: GrantRow }
  | { ok: false; status: number; code: string; message: string };

let rows: GrantRow[] | null = null;
let counter = 0;

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

// A ULID-shaped id that differs per `n`.
function grantId(n: number): string {
  let tail = "";
  let mix = Math.imul(n + 7, 2654435761) >>> 0;
  for (let i = 0; i < 10; i += 1) {
    tail += CROCKFORD[mix % 32];
    mix = (Math.floor(mix / 32) + Math.imul(i + n, 40503)) >>> 0;
  }
  return `01JGR${String(n).padStart(11, "0")}${tail}`;
}

function row(
  n: number,
  kind: GrantRow["kind"],
  conditions: GrantConditions,
  grantedAt: number,
  extra: Partial<GrantRow> = {},
): GrantRow {
  const id = grantId(n);
  return {
    id,
    short: id.slice(-7),
    project_id: "alpha",
    kind,
    conditions,
    granted_at: new Date(grantedAt).toISOString(),
    granted_by: "owner",
    revoked_at: null,
    revoke_reason: null,
    uses: 0,
    ...extra,
  };
}

// 18:00 UTC on the day after `now`: 18:00 Lisbon time in winter.
function tomorrowEvening(now: number): number {
  const day = new Date(now + DAY);
  day.setUTCHours(18, 0, 0, 0);
  return day.getTime();
}

function seed(now: number): GrantRow[] {
  return [
    row(
      5,
      "merge",
      {
        v: 1,
        actions: ["squash"],
        base_ref: "main",
        head_ref_pattern: "alpha-*",
        opened_by_fleet_run: true,
        checks: "success",
        reviews: { require: ["code-review"], max_high: 0 },
        exclude_paths: ["migrations/**"],
        expires_at: new Date(tomorrowEvening(now)).toISOString(),
      },
      now - 2 * DAY,
      { uses: 3 },
    ),
    row(
      3,
      "merge",
      {
        v: 1,
        actions: ["merge", "squash"],
        base_ref: "main",
        checks: "success",
        reviews: { require: ["code-review", "spec-review"] },
      },
      now - 8 * DAY,
      { uses: 11, granted_by: "board" },
    ),
    row(1, "merge", { v: 1, actions: ["merge"], base_ref: "main" }, now - 15 * DAY, {
      uses: 4,
      revoked_at: new Date(now - 4 * DAY).toISOString(),
      revoke_reason: "revoked by the owner",
    }),
  ];
}

function current(): GrantRow[] {
  rows ??= seed(Date.now());
  return rows;
}

// Tests start each case from the seeded grants.
export function resetGrantsFixture(now: number = Date.now()): void {
  rows = seed(now);
  counter = 0;
}

// `GET /api/v1/projects/{id}/grants` for a project this host knows.
export function grantsFor(project: string): ProjectGrants {
  return { project, grants: current().filter((grant) => grant.project_id === project) };
}

function refusal(status: number, code: string, message: string): GrantReply {
  return { ok: false, status, code, message };
}

// A Lisbon wall time, "2026-11-12T18:00", as an instant.
function fromLisbon(field: string): number {
  const guess = Date.parse(`${field}:00Z`);
  const shown = Date.parse(`${lisbonField16(guess)}:00Z`);
  return guess - (shown - guess);
}

// `--expires` as the daemon reads it: RFC 3339, a date (09:00 Lisbon), or a
// date and time without an offset (Lisbon).
function parseWhen(text: string): number | null {
  const input = text.trim();
  if (/^\d{4}-\d{2}-\d{2}$/.test(input)) return fromLisbon(`${input}T09:00`);
  if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(input)) return fromLisbon(input);
  if (/^\d{4}-\d{2}-\d{2}T.+(Z|[+-]\d{2}:\d{2})$/i.test(input)) {
    const at = Date.parse(input);
    return Number.isNaN(at) ? null : at;
  }
  return null;
}

const ACTIONS: Record<string, string[]> = {
  merge: ["merge", "squash", "rebase"],
  land_direct: ["push"],
};

// `POST /api/v1/projects/{project}/grants`, with the checks
// `tiller grant create` makes. `known` says whether this host has the project.
function create(project: string, known: boolean, body: Record<string, unknown>): GrantReply {
  const allowed = new Set(["idempotency_key", "host", "kind", "conditions", "expires"]);
  const unknown = Object.keys(body).find((key) => !allowed.has(key));
  if (unknown) return refusal(400, "usage", `unknown field \`${unknown}\``);
  const kind = String(body.kind);
  const actions = ACTIONS[kind];
  if (!actions) return refusal(400, "usage", `unknown grant kind: ${kind}`);
  const raw = body.conditions;
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    return refusal(400, "usage", "conditions is a JSON object");
  }
  const conditions = { ...(raw as Record<string, unknown>) };
  if (typeof body.expires === "string") {
    const at = parseWhen(body.expires);
    if (at === null) {
      return refusal(
        400,
        "usage",
        `not a time: "${body.expires}" (use 2026-11-07, 2026-11-07T14:00 or an RFC 3339 time with an offset)`,
      );
    }
    conditions.expires_at = new Date(at).toISOString();
  }
  if (!known) return refusal(404, "not_found", `no project ${project}`);
  if (conditions.v !== 1) return refusal(400, "usage", "grant conditions need v: 1");
  const listed = conditions.actions;
  if (
    !Array.isArray(listed) ||
    listed.length === 0 ||
    !listed.every((action) => actions.includes(String(action)))
  ) {
    return refusal(400, "usage", `a ${kind} grant's actions must be some of ${actions.join(", ")}`);
  }
  counter += 1;
  const id = grantId(100 + counter);
  const grant: GrantRow = {
    id,
    short: id.slice(-7),
    project_id: project,
    kind: kind as GrantRow["kind"],
    conditions: conditions as unknown as GrantConditions,
    granted_at: new Date().toISOString(),
    granted_by: "board",
    revoked_at: null,
    revoke_reason: null,
    uses: 0,
  };
  rows = [grant, ...current()];
  return { ok: true, data: grant };
}

// `DELETE /api/v1/grants/{id}`: by its id or short form, and a no-op on a
// grant already revoked.
function revoke(id: string, body: Record<string, unknown>): GrantReply {
  const reason = typeof body.reason === "string" ? body.reason : "revoked by the owner";
  if (reason.trim() === "") return refusal(400, "usage", "reason is required");
  const found = current().find((grant) => grant.id === id || grant.short === id);
  if (!found) return refusal(404, "not_found", `no grant ${id}`);
  if (found.revoked_at) return { ok: true, data: found };
  const revoked = { ...found, revoked_at: new Date().toISOString(), revoke_reason: reason };
  rows = current().map((grant) => (grant.id === found.id ? revoked : grant));
  return { ok: true, data: revoked };
}

// A grant mutation, or `null` when `path` is not one. `known` says whether
// this host has the project a create names.
export function grantMutation(
  method: string,
  path: string,
  body: Record<string, unknown>,
  known: (project: string) => boolean,
): GrantReply | null {
  const made = /^\/api\/v1\/projects\/([^/]+)\/grants$/.exec(path);
  if (method === "POST" && made?.[1]) return create(made[1], known(made[1]), body);
  const ended = /^\/api\/v1\/grants\/([^/]+)$/.exec(path);
  if (method === "DELETE" && ended?.[1]) return revoke(ended[1], body);
  return null;
}
