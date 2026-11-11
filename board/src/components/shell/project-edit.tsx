import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useState } from "react";
import type { RefObject } from "react";

import type { MutationResponse, Project, ProjectChecks, ProjectFields } from "../../lib/api-types";
import { formatLisbonDay } from "../../lib/clock";
import { projectFieldsQuery, projectsPath, queryKeyFor } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { Segmented } from "../ui/segmented";
import { Select } from "../ui/select";
import { MinusIcon, PlusIcon } from "./icons";
import { readFailure } from "./project-dialog";

// `tiller project add`'s delivery modes, in Add project's order.
const DELIVERIES = [
  "direct-pr",
  "local-only",
  "no-mistakes",
  "no-mistakes-prod-only",
  "direct-push",
] as const;

// What a change that touches no logic runs: the full set, a lighter set, or
// nothing (state.md section 3, `checks`).
type Lighter = "same" | "lighter" | "none";

const LIGHTER_MODES: { value: Lighter; label: string }[] = [
  { value: "same", label: "Same checks" },
  { value: "lighter", label: "Lighter set" },
  { value: "none", label: "No checks" },
];

// The form's values, as the owner types them.
interface Draft {
  name: string;
  repo: string;
  defaultBranch: string;
  remote: string;
  delivery: string;
  capacity: number;
  checks: string;
  lighter: Lighter;
  lighterChecks: string;
  deployWorkflow: string;
  timeout: string;
  deployEnvironment: string;
}

type Field = keyof Draft;

// The label each field has in the form, for the conflict message.
const LABELS: Record<Field, string> = {
  name: "Name",
  repo: "Repo",
  defaultBranch: "Base branch",
  remote: "Remote",
  delivery: "Delivery",
  capacity: "Builder slots",
  checks: "Checks that must pass",
  lighter: "Checks for a change with no logic",
  lighterChecks: "Checks for a change with no logic",
  deployWorkflow: "Deploy workflow",
  timeout: "Timeout",
  deployEnvironment: "Deploy environment",
};

// One command per line: the lines with text, trimmed.
function commands(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

function draftOf(fields: ProjectFields): Draft {
  const plan: ProjectChecks = fields.checks ?? [];
  const full = Array.isArray(plan) ? plan : (plan.full ?? []);
  const noLogic = Array.isArray(plan) ? undefined : plan.no_logic;
  return {
    name: fields.name,
    repo: fields.repo ?? "",
    defaultBranch: fields.default_branch,
    remote: fields.remote ?? "",
    delivery: fields.delivery,
    capacity: fields.capacity,
    checks: full.join("\n"),
    lighter: noLogic === undefined ? "same" : noLogic.length === 0 ? "none" : "lighter",
    lighterChecks: (noLogic ?? []).join("\n"),
    deployWorkflow: fields.deploy_workflow ?? "",
    timeout: String(fields.deploy_timeout_minutes),
    deployEnvironment: fields.deploy_environment ?? "",
  };
}

// The `checks` value a draft stores: a plain list while every change runs
// the same commands, else the full set with the lighter one.
function checksOf(draft: Draft): ProjectChecks {
  const full = commands(draft.checks);
  if (draft.lighter === "same") return full;
  return { full, no_logic: draft.lighter === "none" ? [] : commands(draft.lighterChecks) };
}

// A field as it would be stored, so a change of spacing or of a lighter set
// that is the same plan is no change.
function stored(draft: Draft, field: Field): string {
  switch (field) {
    case "checks":
    case "lighter":
    case "lighterChecks":
      return JSON.stringify(checksOf(draft));
    case "timeout":
      return String(Number(draft.timeout.trim()));
    case "capacity":
      return String(draft.capacity);
    default:
      return draft[field].trim();
  }
}

const FIELDS = Object.keys(LABELS) as Field[];

// The fields whose stored value differs between two drafts.
function changedFields(from: Draft, to: Draft): Field[] {
  return FIELDS.filter((field) => stored(from, field) !== stored(to, field));
}

// The PATCH body for what the owner changed. An empty text clears an
// optional field, as an empty `tiller project edit` flag does.
function editOf(base: Draft, draft: Draft): Record<string, unknown> {
  const changed = new Set(changedFields(base, draft));
  const body: Record<string, unknown> = {};
  if (changed.has("name")) body.name = draft.name.trim();
  if (changed.has("repo")) body.repo = draft.repo.trim();
  if (changed.has("defaultBranch")) body.default_branch = draft.defaultBranch.trim();
  if (changed.has("remote")) body.remote = draft.remote.trim();
  if (changed.has("delivery")) body.delivery = draft.delivery;
  if (changed.has("capacity")) body.capacity = draft.capacity;
  if (changed.has("checks")) body.checks = checksOf(draft);
  if (changed.has("deployWorkflow")) body.deploy_workflow = draft.deployWorkflow.trim();
  if (changed.has("timeout")) body.deploy_timeout_minutes = Number(draft.timeout.trim());
  if (changed.has("deployEnvironment")) body.deploy_environment = draft.deployEnvironment.trim();
  return body;
}

// A whole number of minutes, at least 1.
function validTimeout(text: string): boolean {
  return /^\d+$/.test(text.trim()) && Number(text.trim()) >= 1;
}

// "Builder slots is now 2", for the conflict message. The checks fields are
// one line.
function nowLine(field: Field, fresh: Draft): string {
  if (field === "checks" || field === "lighter" || field === "lighterChecks") {
    return "The checks changed.";
  }
  const value = stored(fresh, field);
  if (value === "") return `${LABELS[field]} is now empty.`;
  return `${LABELS[field]} is now ${field === "timeout" ? `${value} min` : value}.`;
}

// What the project looks like now, after a save found it changed since the
// dialog opened.
interface Conflict {
  fresh: ProjectFields;
  lines: string[];
}

// The Edit section of a project's dialog: `tiller project edit`'s fields,
// saved against the `rev` read when the dialog opened.
export function EditProject({
  project,
  first,
  onPending,
  onClose,
  onSaved,
}: {
  project: Project;
  first: RefObject<HTMLInputElement | null>;
  onPending: (pending: boolean) => void;
  onClose: () => void;
  onSaved: (receiptKey: string) => void;
}) {
  const read = useQuery(projectFieldsQuery(project.id));
  // A copy read before the dialog opened may be old: the form waits for this
  // open's own read.
  if (!read.isFetchedAfterMount || !read.data) {
    const failed = readFailure(read.error, "The project could not be read.");
    return (
      <>
        <p className={failed ? "refusal" : "pa-help pa-loading"} role={failed ? "alert" : "status"}>
          {failed ?? "Reading the project..."}
        </p>
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
        </div>
      </>
    );
  }
  return (
    <EditForm
      opened={read.data}
      first={first}
      onPending={onPending}
      onClose={onClose}
      onSaved={onSaved}
    />
  );
}

function EditForm({
  opened,
  first,
  onPending,
  onClose,
  onSaved,
}: {
  opened: ProjectFields;
  first: RefObject<HTMLInputElement | null>;
  onPending: (pending: boolean) => void;
  onClose: () => void;
  onSaved: (receiptKey: string) => void;
}) {
  const queryClient = useQueryClient();
  const ids = useId();
  // What the dialog opened on, or loaded after a conflict: the save goes
  // against its `rev`, and a field is changed when it differs from it.
  const [base, setBase] = useState(opened);
  const baseDraft = draftOf(base);
  const [draft, setDraft] = useState(baseDraft);
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const set = <K extends Field>(field: K, value: Draft[K]) =>
    setDraft((current) => ({ ...current, [field]: value }));

  useEffect(() => {
    first.current?.focus();
  }, [first]);

  const edit = editOf(baseDraft, draft);
  const changed = Object.keys(edit).length > 0;
  const editKey = JSON.stringify(edit);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Project>("PATCH", `/api/v1/projects/${base.id}`, {
        ...(JSON.parse(editKey) as Record<string, unknown>),
        rev: base.rev,
        idempotency_key,
      }),
    [base.id, base.rev, editKey],
  );
  const recorded = useCallback(
    (_: MutationResponse<Project>, receiptKey: string) => {
      void queryClient.invalidateQueries({ queryKey: queryKeyFor(projectsPath) });
      void queryClient.invalidateQueries({
        queryKey: projectFieldsQuery(base.id).queryKey,
      });
      onSaved(receiptKey);
      onClose();
    },
    [base.id, onClose, onSaved, queryClient],
  );
  // The values and the `rev` are the click: a retry of the same save reuses
  // its key, and any other save is a new one.
  const action = useAction({
    binding: `project-edit:${base.id}:${base.rev}:${editKey}`,
    send,
    onRecorded: recorded,
  });
  const { error, pending } = action;
  useEffect(() => onPending(pending), [onPending, pending]);

  // A stale `rev`: read the project again and say which shown fields moved.
  const [handled, setHandled] = useState<typeof error>(null);
  const stale = error?.status === 409 && error !== handled;
  useEffect(() => {
    if (!stale) return;
    let live = true;
    void queryClient
      .fetchQuery({ ...projectFieldsQuery(base.id), staleTime: 0 })
      .then((fresh) => {
        if (!live) return;
        const freshDraft = draftOf(fresh);
        const moved = changedFields(draftOf(base), freshDraft);
        // The three checks fields say one line between them.
        const lines = [...new Set(moved.map((field) => nowLine(field, freshDraft)))];
        setConflict({ fresh, lines });
      })
      .catch(() => {
        if (live) setConflict(null);
      });
    return () => {
      live = false;
    };
  }, [stale, base, queryClient]);

  // The new values: each field that moved takes the stored value, and the
  // owner's other changes stay.
  const load = () => {
    if (!conflict) return;
    const freshDraft = draftOf(conflict.fresh);
    const moved = new Set(changedFields(baseDraft, freshDraft));
    setDraft((current) => {
      const next = { ...current };
      for (const field of moved) (next as Record<Field, unknown>)[field] = freshDraft[field];
      return next;
    });
    setBase(conflict.fresh);
    setConflict(null);
    setHandled(error);
  };

  const nameMissing = draft.name.trim() === "";
  const branchMissing = draft.defaultBranch.trim() === "";
  const timeoutBad = !validTimeout(draft.timeout);
  const valid = !nameMissing && !branchMissing && !timeoutBad;
  const armed = changed && valid && !pending && !stale;
  const busy = pending;

  // When the other change happened, in Lisbon time, from the refusal's
  // `current`. A daemon that predates `edited_at` leaves the time out.
  const editedAt = stale ? error?.current?.edited_at : undefined;
  const when = typeof editedAt === "string" ? formatLisbonDay(editedAt) : null;
  const changedAfter = `${opened.name} changed${when ? ` on ${when},` : ""} after you opened it`;
  const conflictText = conflict
    ? conflict.lines.length > 0
      ? `Not saved: ${changedAfter}. ${conflict.lines.join(" ")} Load the new values, then save again. Your changes to other fields stay.`
      : `Not saved: ${changedAfter}, but none of these fields did. Load the new values, then save again.`
    : null;

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (armed) action.trigger();
      }}
    >
      <div className="pa-fields">
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${ids}-name`}>
            Name
          </label>
          <input
            ref={first}
            id={`${ids}-name`}
            className="note-in"
            autoComplete="off"
            value={draft.name}
            aria-invalid={nameMissing ? true : undefined}
            disabled={busy}
            onChange={(event) => set("name", event.target.value)}
          />
        </div>
        <div className="pa-row">
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${ids}-repo`}>
              Repo <span className="pa-opt">Optional</span>
            </label>
            <input
              id={`${ids}-repo`}
              className="note-in mono"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              placeholder="owner/name"
              value={draft.repo}
              disabled={busy}
              onChange={(event) => set("repo", event.target.value)}
            />
          </div>
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${ids}-branch`}>
              Base branch
            </label>
            <input
              id={`${ids}-branch`}
              className="note-in mono"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              value={draft.defaultBranch}
              aria-invalid={branchMissing ? true : undefined}
              disabled={busy}
              onChange={(event) => set("defaultBranch", event.target.value)}
            />
          </div>
        </div>
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${ids}-remote`}>
            Remote <span className="pa-opt">Optional</span>
          </label>
          <input
            id={`${ids}-remote`}
            className="note-in mono"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            placeholder="https://github.com/owner/name.git"
            value={draft.remote}
            disabled={busy}
            onChange={(event) => set("remote", event.target.value)}
          />
        </div>
        <div className="pa-row">
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${ids}-delivery`}>
              Delivery
            </label>
            <Select
              id={`${ids}-delivery`}
              className="form-select"
              value={draft.delivery}
              disabled={busy}
              onValueChange={(value) => set("delivery", value)}
              options={DELIVERIES.map((value) => ({ value, label: value }))}
            />
          </div>
          <div className="pa-f">
            <span className="pa-l" id={`${ids}-slots`}>
              Builder slots
            </span>
            <div className="pa-num" role="group" aria-labelledby={`${ids}-slots`}>
              <button
                type="button"
                aria-label="One slot fewer"
                disabled={busy || draft.capacity <= 1}
                onClick={() => set("capacity", Math.max(1, draft.capacity - 1))}
              >
                <MinusIcon />
              </button>
              <output aria-live="polite">{draft.capacity}</output>
              <button
                type="button"
                aria-label="One slot more"
                disabled={busy}
                onClick={() => set("capacity", draft.capacity + 1)}
              >
                <PlusIcon />
              </button>
            </div>
          </div>
        </div>
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${ids}-checks`}>
            Checks that must pass
          </label>
          <textarea
            id={`${ids}-checks`}
            className="note-in mono area"
            rows={2}
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            placeholder="cargo test"
            aria-describedby={`${ids}-checks-help`}
            value={draft.checks}
            disabled={busy}
            onChange={(event) => set("checks", event.target.value)}
          />
          <p className="pa-help" id={`${ids}-checks-help`}>
            Commands a direct-push builder runs before it reports done, one per line. Empty means
            none.
          </p>
        </div>
        <div className="pa-f">
          <div className="pa-l-row">
            <span className="pa-l" id={`${ids}-lighter`}>
              For a change with no logic
            </span>
            <Segmented
              className="pa-seg"
              labelledBy={`${ids}-lighter`}
              options={LIGHTER_MODES}
              value={draft.lighter}
              disabled={busy}
              onChange={(value) => set("lighter", value)}
            />
          </div>
          {draft.lighter === "lighter" ? (
            <textarea
              className="note-in mono area"
              rows={2}
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              aria-labelledby={`${ids}-lighter`}
              placeholder="cargo fmt --check"
              value={draft.lighterChecks}
              disabled={busy}
              onChange={(event) => set("lighterChecks", event.target.value)}
            />
          ) : (
            <p className="pa-help pa-help-top">
              {draft.lighter === "same"
                ? "Docs, comments and copy run the same checks."
                : "Docs, comments and copy run no check."}
            </p>
          )}
        </div>
        <div className="pa-row">
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${ids}-workflow`}>
              Deploy workflow <span className="pa-opt">Optional</span>
            </label>
            <input
              id={`${ids}-workflow`}
              className="note-in mono"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              placeholder="deploy.yml"
              value={draft.deployWorkflow}
              disabled={busy}
              onChange={(event) => set("deployWorkflow", event.target.value)}
            />
          </div>
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${ids}-timeout`}>
              Timeout
            </label>
            <span className="pa-unit">
              <input
                id={`${ids}-timeout`}
                className="note-in"
                inputMode="numeric"
                autoComplete="off"
                value={draft.timeout}
                aria-invalid={timeoutBad ? true : undefined}
                disabled={busy}
                onChange={(event) => set("timeout", event.target.value)}
              />
              <span aria-hidden="true">min</span>
            </span>
          </div>
        </div>
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${ids}-environment`}>
            Deploy environment <span className="pa-opt">Optional</span>
          </label>
          <input
            id={`${ids}-environment`}
            className="note-in mono"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            placeholder="production"
            value={draft.deployEnvironment}
            disabled={busy}
            onChange={(event) => set("deployEnvironment", event.target.value)}
          />
        </div>
      </div>
      {stale ? (
        <p className="refusal" role="alert">
          {conflictText ?? `Not saved: ${changedAfter}.`}
        </p>
      ) : error && error !== handled ? (
        <p className="refusal" role="alert">
          {error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button type="button" className="btn" disabled={busy} onClick={onClose}>
          Cancel
        </button>
        {stale ? (
          <button type="button" className="btn primary" disabled={!conflict} onClick={load}>
            Load new values
          </button>
        ) : (
          <button type="submit" className="btn primary" disabled={!armed}>
            {pending ? "Saving..." : "Save"}
          </button>
        )}
      </div>
    </form>
  );
}
