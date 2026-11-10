import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { CSSProperties } from "react";

import type { AwayBrief, AwayBriefResponse, AwayMerge, Project } from "../../lib/api-types";
import { AWAY_BRIEF_ID, MERGE_ACTIONS, mergeLabel } from "../../lib/away";
import { formatLisbon, formatLisbonDay, lisbonField16, serverNow } from "../../lib/clock";
import { ownerOf } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { awayBriefPath, awayBriefQuery, projectsQuery, queryKeyFor } from "../../lib/queries";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { useNamedProjects } from "../board/peer-state";
import { FLEET_PROJECT, ProjectDialog } from "./project-dialog";
import "./away.css";

const MoonIcon = () => (
  <svg className="icon" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M12.8 10.2A5.5 5.5 0 0 1 5.8 3.2a5.5 5.5 0 1 0 7 7z"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinejoin="round"
    />
  </svg>
);

type MergeChoice = "none" | "grants" | "conditions";

interface Form {
  until: string;
  projects: string[];
  orders: string;
  maxRuns: string;
  floor: string;
  merge: MergeChoice;
  actions: string[];
  checks: boolean;
  baseRef: string;
}

// Tomorrow at 18:00 Lisbon time: a day away, ending as the day does.
function tomorrowEvening(): string {
  return `${lisbonField16(serverNow() + 24 * 3_600_000).slice(0, 10)}T18:00`;
}

function freshForm(projects: string[]): Form {
  return {
    until: tomorrowEvening(),
    projects,
    orders: "",
    maxRuns: "6",
    floor: "20",
    merge: "grants",
    actions: ["merge"],
    checks: true,
    baseRef: "",
  };
}

// The dialog filled with the running contract, to edit it in place.
function formOf(brief: AwayBrief): Form {
  const conditions = typeof brief.merge === "object" ? brief.merge : null;
  return {
    until: lisbonField16(Date.parse(brief.until_at)),
    projects: brief.projects,
    orders: brief.orders ?? "",
    maxRuns: brief.spend.max_new_runs === null ? "" : String(brief.spend.max_new_runs),
    floor: brief.spend.quota_floor_percent === null ? "" : String(brief.spend.quota_floor_percent),
    merge: conditions ? "conditions" : brief.merge === "none" ? "none" : "grants",
    actions: Array.isArray(conditions?.actions) ? (conditions.actions as string[]) : ["merge"],
    checks: conditions ? conditions.checks === "success" : true,
    baseRef: typeof conditions?.base_ref === "string" ? conditions.base_ref : "",
  };
}

function mergeOf(form: Form): AwayMerge {
  if (form.merge !== "conditions") return form.merge;
  return {
    v: 1,
    actions: form.actions,
    ...(form.checks ? { checks: "success" } : {}),
    ...(form.baseRef.trim() ? { base_ref: form.baseRef.trim() } : {}),
  };
}

function wholeNumber(text: string, min: number, max: number): number | null | undefined {
  if (text.trim() === "") return null;
  const value = Number(text);
  return Number.isInteger(value) && value >= min && value <= max ? value : undefined;
}

// What is wrong with the form, said as the dialog says it; null when it can go.
function problemOf(form: Form): string | null {
  const until = Date.parse(`${form.until}:00+00:00`);
  if (!form.until || Number.isNaN(until)) return "Choose when away mode ends.";
  if (form.until <= lisbonField16(serverNow())) return "That time has passed. Choose a later one.";
  if (form.projects.length === 0) return "Choose at least one project.";
  if (wholeNumber(form.maxRuns, 1, 10_000) === undefined) {
    return "New runs, at most: use a whole number, or leave it empty.";
  }
  if (wholeNumber(form.floor, 0, 100) === undefined) {
    return "Quota floor: use a whole number from 0 to 100, or leave it empty.";
  }
  if (form.merge === "conditions" && form.actions.length === 0) {
    return "Choose at least one way a grant may merge.";
  }
  return null;
}

function bodyOf(form: Form) {
  const orders = form.orders.trim();
  const maxRuns = wholeNumber(form.maxRuns, 1, 10_000);
  const floor = wholeNumber(form.floor, 0, 100);
  return {
    until: form.until,
    projects: form.projects,
    ...(orders ? { orders } : {}),
    ...(maxRuns ? { max_new_runs: maxRuns } : {}),
    ...(floor !== null && floor !== undefined ? { quota_floor_percent: floor } : {}),
    merge: mergeOf(form),
  };
}

function toggled(list: string[], value: string): string[] {
  return list.includes(value) ? list.filter((item) => item !== value) : [...list, value];
}

function AwayDialog({
  projects,
  editing,
  onClose,
}: {
  projects: Project[];
  editing: AwayBrief | null;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const id = useId();
  const [form, setForm] = useState<Form>(() =>
    editing ? formOf(editing) : freshForm(projects.map((project) => project.id)),
  );
  const [shown, setShown] = useState(false);
  const until = useRef<HTMLInputElement>(null);
  const set = (change: Partial<Form>) => setForm((current) => ({ ...current, ...change }));
  const problem = problemOf(form);
  // The body is a pure function of the form, so its text keys a retry.
  const bodyText = JSON.stringify(bodyOf(form));

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation(editing ? "PUT" : "POST", "/api/v1/away", {
        ...(JSON.parse(bodyText) as Record<string, unknown>),
        idempotency_key,
      }),
    [editing, bodyText],
  );
  const recorded = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: queryKeyFor(awayBriefPath) });
    onClose();
  }, [onClose, queryClient]);
  const action = useAction({
    binding: `away:${editing ? "edit" : "start"}:${bodyText}`,
    send,
    onRecorded: recorded,
  });
  const failure = shown ? problem : null;

  return (
    <ProjectDialog
      name="away"
      className="away-dialog"
      pending={action.pending}
      onClose={onClose}
      initial={until}
    >
      <h2 id="away-dialog-title">{editing ? "Edit away mode" : "Start away mode"}</h2>
      <p id="away-dialog-body">
        Orchestrators keep working inside these limits. Away never answers a decision for you.
      </p>
      <div className="pa-fields">
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${id}-until`}>
            Until
          </label>
          <input
            ref={until}
            id={`${id}-until`}
            type="datetime-local"
            className="note-in"
            value={form.until}
            onChange={(event) => set({ until: event.target.value })}
          />
          <p className="pa-help">Lisbon time. Away ends by itself then.</p>
        </div>
        <fieldset className="pa-f away-fieldset">
          <legend className="pa-l">Projects</legend>
          <div className="away-chips">
            {projects.map((project, index) => (
              <label key={project.id} className="away-chip">
                <input
                  type="checkbox"
                  checked={form.projects.includes(project.id)}
                  onChange={() => set({ projects: toggled(form.projects, project.id) })}
                />
                <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
                {project.name}
              </label>
            ))}
          </div>
        </fieldset>
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${id}-orders`}>
            Orders<span className="pa-opt">optional</span>
          </label>
          <textarea
            id={`${id}-orders`}
            className="note-in area"
            value={form.orders}
            placeholder="What the orchestrators should do, or leave alone, while you are gone."
            onChange={(event) => set({ orders: event.target.value })}
          />
        </div>
        <div className="away-row">
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${id}-runs`}>
              New runs, at most
            </label>
            <input
              id={`${id}-runs`}
              className="note-in"
              inputMode="numeric"
              value={form.maxRuns}
              placeholder="No limit"
              onChange={(event) => set({ maxRuns: event.target.value })}
            />
          </div>
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${id}-floor`}>
              Quota floor, %
            </label>
            <input
              id={`${id}-floor`}
              className="note-in"
              inputMode="numeric"
              value={form.floor}
              placeholder="No floor"
              onChange={(event) => set({ floor: event.target.value })}
            />
          </div>
        </div>
        <div className="pa-f">
          <span className="pa-l" id={`${id}-merges`}>
            Merges
          </span>
          <div className="seg away-seg" role="radiogroup" aria-labelledby={`${id}-merges`}>
            {(
              [
                ["none", "None"],
                ["grants", "Existing grants"],
                ["conditions", "Conditions..."],
              ] as const
            ).map(([value, label]) => (
              <button
                key={value}
                type="button"
                role="radio"
                aria-checked={form.merge === value}
                aria-pressed={form.merge === value}
                onClick={() => set({ merge: value })}
              >
                {label}
              </button>
            ))}
          </div>
          <p className="pa-help">
            Only the grants you already have may merge. &quot;Conditions&quot; adds a grant that
            ends with away mode.
          </p>
          {form.merge === "conditions" ? (
            <div className="away-cond" role="group" aria-label="Grant conditions">
              <div className="away-chips">
                {MERGE_ACTIONS.map((name) => (
                  <label key={name} className="away-chip">
                    <input
                      type="checkbox"
                      checked={form.actions.includes(name)}
                      onChange={() => set({ actions: toggled(form.actions, name) })}
                    />
                    {name}
                  </label>
                ))}
                <label className="away-chip">
                  <input
                    type="checkbox"
                    checked={form.checks}
                    onChange={() => set({ checks: !form.checks })}
                  />
                  Checks must pass
                </label>
              </div>
              <label className="pa-l" htmlFor={`${id}-base`}>
                Base branch<span className="pa-opt">optional</span>
              </label>
              <input
                id={`${id}-base`}
                className="note-in mono"
                value={form.baseRef}
                placeholder="main"
                onChange={(event) => set({ baseRef: event.target.value })}
              />
            </div>
          ) : null}
        </div>
      </div>
      {failure ? (
        <p className="refusal" role="alert">
          {failure}
        </p>
      ) : null}
      {action.error ? (
        <p className="refusal" role="alert">
          {action.error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button type="button" className="btn" disabled={action.pending} onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={action.pending}
          onClick={() => {
            setShown(true);
            if (!problem) action.trigger();
          }}
        >
          {action.pending
            ? editing
              ? "Saving..."
              : "Starting..."
            : editing
              ? "Save changes"
              : "Start away mode"}
        </button>
      </div>
    </ProjectDialog>
  );
}

// "Alpha, Beta", by the project names the contract covers.
function namesOf(ids: string[], projects: Project[]): string {
  return ids.map((id) => projects.find((project) => project.id === id)?.name ?? id).join(", ");
}

function AwayPopover({
  brief,
  projects,
  onEdit,
  onEnd,
  ending,
  error,
}: {
  brief: AwayBrief;
  projects: Project[];
  onEdit: () => void;
  onEnd: () => void;
  ending: boolean;
  error: string | null;
}) {
  const { spend } = brief;
  return (
    <div className="away-pop" role="dialog" aria-label="Away mode">
      <div className="away-pop-b">
        <b>Away until {formatLisbonDay(brief.until_at)}</b>
        <dl className="props away-props">
          <dt>Projects</dt>
          <dd>{namesOf(brief.projects, projects)}</dd>
          <dt>Runs</dt>
          <dd>
            {spend.max_new_runs === null
              ? `${spend.new_runs} started`
              : `${spend.new_runs} of ${spend.max_new_runs}`}
          </dd>
          <dt>Merges</dt>
          <dd>{mergeLabel(brief.merge)}</dd>
        </dl>
        {error ? (
          <p className="refusal" role="alert">
            {error}
          </p>
        ) : null}
      </div>
      <div className="away-pop-f">
        <button type="button" className="btn sm" disabled={ending} onClick={onEdit}>
          Edit
        </button>
        <button type="button" className="btn sm primary" disabled={ending} onClick={onEnd}>
          {ending ? "Ending..." : "End"}
        </button>
      </div>
    </div>
  );
}

// The Away control in the header, left of Ask. Idle it is a button that opens
// the Start dialog. While away it is a status, and a popover holds the limits
// with Edit and End. End asks nothing: it only gives control back, and opens
// the brief.
export function AwayControl() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const brief = useQuery(awayBriefQuery()).data?.brief ?? null;
  const projects = useNamedProjects(useQuery(projectsQuery()).data)?.projects ?? [];
  // Away covers this host's projects; `fleet` is not one an owner is away from.
  const mine = projects.filter(
    (project) => ownerOf(project) === undefined && project.id !== FLEET_PROJECT,
  );
  const [dialog, setDialog] = useState<"start" | "edit" | null>(null);
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLSpanElement>(null);
  const away = brief?.away ? brief : null;

  const closeDialog = useCallback(() => setDialog(null), []);
  const end = useAction<{ brief: AwayBrief | null }>({
    binding: `away:end:${away?.approval_id ?? ""}`,
    send: useCallback(
      (idempotency_key: string) =>
        sendMutation<{ brief: AwayBrief | null }>("DELETE", "/api/v1/away", { idempotency_key }),
      [],
    ),
    onRecorded: useCallback(
      (response: { data: { brief: AwayBrief | null } }) => {
        queryClient.setQueryData<AwayBriefResponse>(queryKeyFor(awayBriefPath), {
          brief: response.data.brief,
        });
        void queryClient.invalidateQueries({ queryKey: queryKeyFor(awayBriefPath) });
        setOpen(false);
        void navigate({ to: "/inbox", search: { item: AWAY_BRIEF_ID } });
      },
      [navigate, queryClient],
    ),
  });

  // The popover closes on a click elsewhere and on Escape.
  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (!wrap.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !shortcutsBlocked()) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <span className="away-wrap" ref={wrap}>
      {away ? (
        <button
          type="button"
          className="btn away-pill"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={`Away until ${formatLisbonDay(away.until_at)}`}
          onClick={() => setOpen((current) => !current)}
        >
          <i className="ld live" />
          <span className="away-when">Away until {formatLisbon(away.until_at)}</span>
        </button>
      ) : (
        <button
          type="button"
          className="btn away-open"
          disabled={mine.length === 0}
          onClick={() => setDialog("start")}
        >
          <MoonIcon />
          Away
        </button>
      )}
      {away && open ? (
        <AwayPopover
          brief={away}
          projects={projects}
          ending={end.pending}
          error={end.error?.message ?? null}
          onEdit={() => {
            setOpen(false);
            setDialog("edit");
          }}
          onEnd={end.trigger}
        />
      ) : null}
      {dialog ? (
        <AwayDialog
          projects={mine}
          editing={dialog === "edit" ? away : null}
          onClose={closeDialog}
        />
      ) : null}
    </span>
  );
}
