import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";

import type { GrantRow, MutationResponse, Project } from "../../lib/api-types";
import { MERGE_ACTIONS } from "../../lib/away";
import { formatLisbonDay, lisbonField16, serverNow } from "../../lib/clock";
import { projectFieldsQuery, projectGrantsQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { InlineReceipt } from "../attention/receipt";
import { ulid } from "../attention/ulid";
import { useAction } from "../attention/use-action";
import { lisbonShortDay } from "../health/lisbon";
import { Segmented } from "../ui/segmented";
import { PlusIcon } from "./icons";
import { ProjectDialog, readFailure } from "./project-dialog";
import "./project-grants.css";

// The reviewers a grant can require (authority.md section 6).
const REVIEWERS = ["code-review", "spec-review", "no-mistakes", "human"] as const;

// The ended grants the dialog lists under the live ones, newest first.
const ENDED_SHOWN = 5;

// A grant still matches: not revoked, and not past its end.
function isLive(grant: GrantRow, now: number): boolean {
  if (grant.revoked_at) return false;
  const end = grant.conditions.expires_at;
  return !end || Date.parse(end) > now;
}

// "squash", "merge or squash", "merge, squash or rebase".
function either(words: string[]): string {
  if (words.length <= 1) return words.join("");
  return `${words.slice(0, -1).join(", ")} or ${words.at(-1)}`;
}

function capitalized(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// What a grant lets land, in a few words: "Squash into main".
function grantTitle(grant: GrantRow): string {
  const base = grant.conditions.base_ref;
  if (grant.kind === "land_direct") return base ? `Direct push to ${base}` : "Direct push";
  const how = capitalized(either(grant.conditions.actions ?? []));
  return base ? `${how} into ${base}` : `${how} into any branch`;
}

function Code({ children }: { children: string }) {
  return <code>{children}</code>;
}

// Code spans joined as "a, b or c".
function codeList(items: string[]): ReactNode[] {
  return items.flatMap((item, index) => [
    index === 0 ? null : index === items.length - 1 ? " or " : ", ",
    <Code key={item}>{item}</Code>,
  ]);
}

// Each condition of a grant as one plain line. A key the grant leaves out
// does not limit it, so it has no line.
function conditionLines(grant: GrantRow, projectName: string): ReactNode[] {
  const c = grant.conditions;
  if (grant.kind === "land_direct") {
    return ["The project's own checks still run before the push"];
  }
  const lines: ReactNode[] = [];
  const base = c.base_ref;
  const head = c.head_ref_pattern;
  if (base || head) {
    lines.push(
      <>
        {base ? (
          <>
            Base <Code>{base}</Code>
          </>
        ) : (
          "Any base"
        )}
        {head ? (
          <>
            , head <Code>{head}</Code>
          </>
        ) : (
          ", any head"
        )}
      </>,
    );
  }
  if (c.opened_by_fleet_run) lines.push(`Only PRs a builder of ${projectName} opened`);
  if (c.card_kinds?.length) lines.push(`Only ${either(c.card_kinds)} cards`);
  if (c.checks === "success") lines.push("Every check on the PR must pass");
  if (c.pipeline === "passed") lines.push("The no-mistakes pipeline must pass");
  const required = c.reviews?.require ?? [];
  if (required.length > 0) {
    const max = c.reviews?.max_high;
    const findings =
      max === 0
        ? ", with no high finding"
        : typeof max === "number"
          ? `, with at most ${max} high ${max === 1 ? "finding" : "findings"}`
          : "";
    lines.push(
      <>
        {codeList(required)} must pass on the current PR head{findings}
      </>,
    );
  }
  if (c.exclude_paths?.length) {
    lines.push(<>Never touches {codeList(c.exclude_paths)} or the source-map paths</>);
  }
  return lines;
}

// "you", "you on host-b", or "away mode".
function madeBy(grant: GrantRow): string {
  if (grant.conditions.away_approval_id) return "away mode";
  const peer = /^board@(.+)$/.exec(grant.granted_by);
  if (peer) return `you on ${peer[1]}`;
  return grant.granted_by === "daemon" ? "tiller" : "you";
}

function usesText(uses: number): string {
  if (uses === 0) return "not used yet";
  return uses === 1 ? "used once" : `used ${uses} times`;
}

// "Ends Thu 12 Nov, 18:00 · used 3 times · made by you, Mon 9 Nov".
function metaLine(grant: GrantRow, now: number): string {
  const end = grant.conditions.expires_at ?? null;
  const ended = grant.revoked_at
    ? `Revoked ${lisbonShortDay(grant.revoked_at)}`
    : end
      ? `${Date.parse(end) > now ? "Ends" : "Ended"} ${formatLisbonDay(end)}`
      : "No end date";
  return [
    ended,
    usesText(grant.uses),
    `made by ${madeBy(grant)}, ${lisbonShortDay(grant.granted_at)}`,
  ].join(" · ");
}

// The Grants fact on a project's panel: "2 active", then "1 ends Thu 12 Nov"
// muted beside it.
export function GrantsLine({ grants }: { grants: GrantRow[] }) {
  const now = serverNow();
  const live = grants.filter((grant) => isLive(grant, now));
  if (live.length === 0) return <>none</>;
  const ends = live
    .map((grant) => grant.conditions.expires_at)
    .filter((end): end is string => !!end);
  // The soonest end: every `expires_at` is an RFC 3339 time in UTC.
  const soonest = ends.reduce<string | null>(
    (first, end) => (first === null || end < first ? end : first),
    null,
  );
  return (
    <>
      <span>{live.length} active</span>
      {soonest ? (
        <span className="stage-when">
          {ends.length === 1 ? "1 ends" : `${ends.length} end, the first`} {lisbonShortDay(soonest)}
        </span>
      ) : null}
    </>
  );
}

function GrantItem({
  grant,
  projectName,
  now,
  onRevoke,
}: {
  grant: GrantRow;
  projectName: string;
  now: number;
  onRevoke?: () => void;
}) {
  const titleId = useId();
  const lines = conditionLines(grant, projectName);
  return (
    <li className="gr-row">
      <div className="gr-body">
        <span className="gr-title" id={titleId}>
          <b>{grantTitle(grant)}</b>
          <code className="gr-id">{grant.short}</code>
        </span>
        {lines.length > 0 ? (
          <ul>
            {lines.map((line, index) => (
              <li key={index}>{line}</li>
            ))}
          </ul>
        ) : null}
        <span className="gr-meta">{metaLine(grant, now)}</span>
      </div>
      {onRevoke ? (
        <button type="button" className="btn sm" aria-describedby={titleId} onClick={onRevoke}>
          Revoke...
        </button>
      ) : null}
    </li>
  );
}

// The Grants tab of a project's dialog: every grant of the project, the live
// ones first, as `tiller grant create` and `tiller grant revoke` manage them.
export function GrantsTab({
  project,
  first,
  onClose,
}: {
  project: Project;
  first: RefObject<HTMLButtonElement | null>;
  onClose: () => void;
}) {
  const read = useQuery(projectGrantsQuery(project.id));
  const [making, setMaking] = useState(false);
  const [revoking, setRevoking] = useState<GrantRow | null>(null);
  const [receipt, setReceipt] = useState<{ key: string; text: string } | null>(null);
  const now = serverNow();
  const grants = read.data?.grants ?? [];
  const live = grants.filter((grant) => isLive(grant, now));
  const ended = grants.filter((grant) => !isLive(grant, now)).slice(0, ENDED_SHOWN);
  const failed = readFailure(read.error, "The grants could not be read.");
  const headBound = live.some((grant) => (grant.conditions.reviews?.require ?? []).length > 0);

  const closeNew = useCallback(() => setMaking(false), []);
  const closeRevoke = useCallback(() => setRevoking(null), []);
  const made = useCallback((key: string, text: string) => setReceipt({ key, text }), []);

  return (
    <>
      <p id="project-settings-dialog-body" className="gr-intro">
        A PR that meets every line of one grant may merge without asking you.
      </p>
      <div className="gr-head">
        <h3>Active grants</h3>
        {read.data ? <span className="gr-count">{live.length}</span> : null}
        <span className="gr-sp" />
        <button ref={first} type="button" className="btn sm" onClick={() => setMaking(true)}>
          <PlusIcon />
          New grant
        </button>
      </div>
      {!read.data ? (
        <p className={failed ? "refusal" : "pa-help pa-loading"} role={failed ? "alert" : "status"}>
          {failed ?? "Reading the grants..."}
        </p>
      ) : live.length === 0 ? (
        <p className="gr-empty">No grant is active, so every merge asks you first.</p>
      ) : (
        <ul className="gr-list" aria-label="Active grants">
          {live.map((grant) => (
            <GrantItem
              key={grant.id}
              grant={grant}
              projectName={project.name}
              now={now}
              onRevoke={() => setRevoking(grant)}
            />
          ))}
        </ul>
      )}
      {headBound ? (
        <p className="gr-note">
          A new push to the PR makes an old review stale, so the grant waits for a fresh one.
        </p>
      ) : null}
      {ended.length > 0 ? (
        <>
          <h3 className="gr-sub">Ended</h3>
          <ul className="gr-list gr-ended" aria-label="Ended grants">
            {ended.map((grant) => (
              <GrantItem key={grant.id} grant={grant} projectName={project.name} now={now} />
            ))}
          </ul>
        </>
      ) : null}
      {receipt ? (
        <div className="gr-receipt">
          <InlineReceipt receiptKey={receipt.key} text={receipt.text} />
        </div>
      ) : null}
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose}>
          Close
        </button>
      </div>
      {making ? <NewGrant project={project} onClose={closeNew} onMade={made} /> : null}
      {revoking ? <RevokeGrant grant={revoking} onClose={closeRevoke} onRevoked={made} /> : null}
    </>
  );
}

function toggled(list: string[], item: string): string[] {
  return list.includes(item) ? list.filter((each) => each !== item) : [...list, item];
}

// Globs typed with commas or spaces between them.
function globs(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((glob) => glob.trim())
    .filter((glob) => glob !== "");
}

interface Draft {
  kind: GrantRow["kind"];
  actions: string[];
  base: string;
  head: string;
  fleet: boolean;
  checks: boolean;
  pipeline: boolean;
  reviewers: string[];
  noHigh: "none" | "any";
  exclude: string;
  ends: string;
}

// The conditions object a draft sends (authority.md section 6). A
// `land_direct` grant checks no PR, so it carries only its action.
function conditionsOf(draft: Draft): Record<string, unknown> {
  if (draft.kind === "land_direct") return { v: 1, actions: ["push"] };
  const conditions: Record<string, unknown> = { v: 1, actions: draft.actions };
  if (draft.base.trim()) conditions.base_ref = draft.base.trim();
  if (draft.head.trim()) conditions.head_ref_pattern = draft.head.trim();
  if (draft.fleet) conditions.opened_by_fleet_run = true;
  if (draft.checks) conditions.checks = "success";
  if (draft.pipeline) conditions.pipeline = "passed";
  if (draft.reviewers.length > 0) {
    conditions.reviews = {
      require: draft.reviewers,
      ...(draft.noHigh === "none" ? { max_high: 0 } : {}),
    };
  }
  const excluded = globs(draft.exclude);
  if (excluded.length > 0) conditions.exclude_paths = excluded;
  return conditions;
}

// What keeps the draft from being sent, as the dialog says it.
function problemOf(draft: Draft): string | null {
  if (draft.kind === "merge" && draft.actions.length === 0) {
    return "Choose at least one way the grant may merge.";
  }
  if (draft.ends && draft.ends <= lisbonField16(serverNow())) {
    return "That end time has passed. Choose a later one, or leave it empty.";
  }
  return null;
}

// New grant: `tiller grant create`'s conditions as fields.
function NewGrant({
  project,
  onClose,
  onMade,
}: {
  project: Project;
  onClose: () => void;
  onMade: (receiptKey: string, text: string) => void;
}) {
  const fields = useQuery(projectFieldsQuery(project.id));
  const cancel = useRef<HTMLButtonElement>(null);
  const [pending, setPending] = useState(false);
  return (
    <ProjectDialog
      name="grant-new"
      className="pa-dialog"
      pending={pending}
      onClose={onClose}
      initial={cancel}
    >
      <h2 id="grant-new-dialog-title">New grant for {project.name}</h2>
      <p id="grant-new-dialog-body">
        A PR that meets every line below may merge without asking you.
      </p>
      {fields.isFetched ? (
        <GrantForm
          project={project}
          baseBranch={fields.data?.default_branch ?? ""}
          cancel={cancel}
          onPending={setPending}
          onClose={onClose}
          onMade={onMade}
        />
      ) : (
        <>
          <p className="pa-help pa-loading" role="status">
            Reading the project...
          </p>
          <div className="dialog-actions">
            <button ref={cancel} type="button" className="btn" onClick={onClose}>
              Cancel
            </button>
          </div>
        </>
      )}
    </ProjectDialog>
  );
}

function GrantForm({
  project,
  baseBranch,
  cancel,
  onPending,
  onClose,
  onMade,
}: {
  project: Project;
  baseBranch: string;
  cancel: RefObject<HTMLButtonElement | null>;
  onPending: (pending: boolean) => void;
  onClose: () => void;
  onMade: (receiptKey: string, text: string) => void;
}) {
  const queryClient = useQueryClient();
  const ids = useId();
  // Each opening of the form is its own grant: making the same one twice is
  // two grants, while a retry of one send reuses its key.
  const [opening] = useState(ulid);
  const direct = project.delivery === "direct-push";
  const pipelined = project.delivery.startsWith("no-mistakes");
  const [draft, setDraft] = useState<Draft>({
    kind: "merge",
    actions: ["merge"],
    base: baseBranch,
    head: "",
    fleet: true,
    checks: true,
    pipeline: pipelined,
    reviewers: [],
    noHigh: "none",
    exclude: "",
    ends: "",
  });
  const set = (patch: Partial<Draft>) => setDraft((current) => ({ ...current, ...patch }));

  const body = JSON.stringify({
    kind: draft.kind,
    conditions: conditionsOf(draft),
    ...(draft.ends ? { expires: draft.ends } : {}),
  });
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<GrantRow>("POST", `/api/v1/projects/${project.id}/grants`, {
        ...(JSON.parse(body) as Record<string, unknown>),
        idempotency_key,
      }),
    [body, project.id],
  );
  const recorded = useCallback(
    (_: MutationResponse<GrantRow>, receiptKey: string) => {
      void queryClient.invalidateQueries({ queryKey: projectGrantsQuery(project.id).queryKey });
      onMade(receiptKey, "Grant made");
      onClose();
    },
    [onClose, onMade, project.id, queryClient],
  );
  const action = useAction({ binding: `grant-new:${opening}:${body}`, send, onRecorded: recorded });
  const { pending, error } = action;
  useEffect(() => onPending(pending), [onPending, pending]);
  const problem = problemOf(draft);
  const merge = draft.kind === "merge";
  const form = useRef<HTMLFormElement>(null);
  // The form takes the place of the read's Cancel, which had focus: the
  // first field takes it instead.
  useEffect(() => {
    const active = document.activeElement;
    if (active && active !== document.body && active.isConnected) return;
    form.current?.querySelector<HTMLElement>("fieldset input, fieldset button")?.focus();
  }, []);

  return (
    <form
      ref={form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!problem && !pending) action.trigger();
      }}
    >
      <fieldset className="pa-fields gr-fieldset" disabled={pending}>
        {direct ? (
          <div className="pa-f">
            <span className="pa-l" id={`${ids}-kind`}>
              Lets land
            </span>
            <Segmented
              className="pa-seg gr-seg"
              labelledBy={`${ids}-kind`}
              options={[
                { value: "merge" as const, label: "A merged PR" },
                { value: "land_direct" as const, label: "A direct push" },
              ]}
              value={draft.kind}
              onChange={(kind) => set({ kind })}
            />
          </div>
        ) : null}
        {merge ? (
          <>
            <fieldset className="pa-f gr-fieldset">
              <legend className="pa-l">May merge by</legend>
              <div className="gr-chips">
                {MERGE_ACTIONS.map((name) => (
                  <label key={name} className="gr-chip">
                    <input
                      type="checkbox"
                      checked={draft.actions.includes(name)}
                      onChange={() => set({ actions: toggled(draft.actions, name) })}
                    />
                    {name}
                  </label>
                ))}
              </div>
            </fieldset>
            <div className="gr-pair">
              <div className="pa-f">
                <label className="pa-l" htmlFor={`${ids}-base`}>
                  Base branch <span className="pa-opt">Optional</span>
                </label>
                <input
                  id={`${ids}-base`}
                  className="note-in mono"
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  placeholder="any"
                  value={draft.base}
                  onChange={(event) => set({ base: event.target.value })}
                />
              </div>
              <div className="pa-f">
                <label className="pa-l" htmlFor={`${ids}-head`}>
                  Head <span className="pa-opt">Optional</span>
                </label>
                <input
                  id={`${ids}-head`}
                  className="note-in mono"
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  placeholder={`${project.id}-*`}
                  value={draft.head}
                  onChange={(event) => set({ head: event.target.value })}
                />
              </div>
            </div>
            <fieldset className="pa-f gr-fieldset">
              <legend className="pa-l">The PR must</legend>
              <div className="gr-checks">
                <label className="gr-check">
                  <input
                    type="checkbox"
                    checked={draft.fleet}
                    onChange={() => set({ fleet: !draft.fleet })}
                  />
                  Come from a builder of {project.name}
                </label>
                <label className="gr-check">
                  <input
                    type="checkbox"
                    checked={draft.checks}
                    onChange={() => set({ checks: !draft.checks })}
                  />
                  Pass every check
                </label>
                {pipelined ? (
                  <label className="gr-check">
                    <input
                      type="checkbox"
                      checked={draft.pipeline}
                      onChange={() => set({ pipeline: !draft.pipeline })}
                    />
                    Pass the no-mistakes pipeline
                  </label>
                ) : null}
              </div>
            </fieldset>
            <fieldset className="pa-f gr-fieldset">
              <legend className="pa-l">
                Reviews that must pass <span className="pa-opt">Optional</span>
              </legend>
              <div className="gr-chips">
                {REVIEWERS.map((name) => (
                  <label key={name} className="gr-chip">
                    <input
                      type="checkbox"
                      checked={draft.reviewers.includes(name)}
                      onChange={() => set({ reviewers: toggled(draft.reviewers, name) })}
                    />
                    {name}
                  </label>
                ))}
              </div>
              {draft.reviewers.length > 0 ? (
                <>
                  <div className="pa-l-row gr-high">
                    <span className="pa-l" id={`${ids}-high`}>
                      High findings allowed
                    </span>
                    <Segmented
                      className="pa-seg gr-seg"
                      labelledBy={`${ids}-high`}
                      options={[
                        { value: "none" as const, label: "None" },
                        { value: "any" as const, label: "Any" },
                      ]}
                      value={draft.noHigh}
                      onChange={(noHigh) => set({ noHigh })}
                    />
                  </div>
                  <p className="pa-help">
                    A review counts only on the PR&apos;s current head. A new push waits for a fresh
                    one.
                  </p>
                </>
              ) : null}
            </fieldset>
            <div className="pa-f">
              <label className="pa-l" htmlFor={`${ids}-exclude`}>
                Never touches <span className="pa-opt">Optional</span>
              </label>
              <input
                id={`${ids}-exclude`}
                className="note-in mono"
                autoComplete="off"
                autoCapitalize="off"
                spellCheck={false}
                placeholder="migrations/**"
                aria-describedby={`${ids}-exclude-help`}
                value={draft.exclude}
                onChange={(event) => set({ exclude: event.target.value })}
              />
              <p className="pa-help" id={`${ids}-exclude-help`}>
                Paths, with a comma between them. The agent instruction files and the source-map
                paths are always left out.
              </p>
            </div>
          </>
        ) : (
          <p className="pa-help">
            A direct push still runs the project&apos;s own checks before it lands.
          </p>
        )}
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${ids}-ends`}>
            Ends <span className="pa-opt">Optional</span>
          </label>
          <input
            id={`${ids}-ends`}
            type="datetime-local"
            className="note-in"
            aria-describedby={`${ids}-ends-help`}
            aria-invalid={problem && draft.ends ? true : undefined}
            value={draft.ends}
            onChange={(event) => set({ ends: event.target.value })}
          />
          <p className="pa-help" id={`${ids}-ends-help`}>
            Lisbon time. Empty means the grant stays until you revoke it.
          </p>
        </div>
      </fieldset>
      {problem ? (
        <p className="refusal" role="alert">
          {problem}
        </p>
      ) : error ? (
        <p className="refusal" role="alert">
          {error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button ref={cancel} type="button" className="btn" disabled={pending} onClick={onClose}>
          Cancel
        </button>
        <button type="submit" className="btn primary" disabled={problem !== null || pending}>
          {pending ? "Making..." : "Create grant"}
        </button>
      </div>
    </form>
  );
}

// Revoke: destructive, so the dialog asks first, and Keep takes focus.
function RevokeGrant({
  grant,
  onClose,
  onRevoked,
}: {
  grant: GrantRow;
  onClose: () => void;
  onRevoked: (receiptKey: string, text: string) => void;
}) {
  const queryClient = useQueryClient();
  const keep = useRef<HTMLButtonElement>(null);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<GrantRow>("DELETE", `/api/v1/grants/${grant.id}`, { idempotency_key }),
    [grant.id],
  );
  const recorded = useCallback(
    (_: MutationResponse<GrantRow>, receiptKey: string) => {
      void queryClient.invalidateQueries({
        queryKey: projectGrantsQuery(grant.project_id).queryKey,
      });
      onRevoked(receiptKey, "Grant revoked");
      onClose();
    },
    [grant.project_id, onClose, onRevoked, queryClient],
  );
  const action = useAction({ binding: `grant-revoke:${grant.id}`, send, onRecorded: recorded });
  const title = grantTitle(grant);
  const started = grant.kind === "land_direct" ? "A push" : "A merge";
  return (
    <ProjectDialog name="grant-revoke" pending={action.pending} onClose={onClose} initial={keep}>
      <h2 id="grant-revoke-dialog-title">Revoke grant {grant.short}?</h2>
      <p id="grant-revoke-dialog-body">
        PRs that wait on &quot;{title}&quot; go back to you for approval. {started} that already
        started is not stopped.
      </p>
      {action.error ? (
        <p className="refusal" role="alert">
          {action.error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Keep grant
        </button>
        <button
          type="button"
          className="btn danger"
          disabled={action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Revoking..." : "Revoke grant"}
        </button>
      </div>
    </ProjectDialog>
  );
}
