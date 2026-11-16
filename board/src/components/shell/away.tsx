import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { CSSProperties } from "react";

import type {
  AwayBrief,
  AwayBriefResponse,
  AwayMerge,
  MutationResponse,
  Project,
} from "../../lib/api-types";
import {
  AWAY_BRIEF_ID,
  MERGE_ACTIONS,
  awayProjects,
  contractKeys,
  earliestUntil,
  mergeLabel,
  projectName,
  runningOf,
  unreadHosts,
} from "../../lib/away";
import type { HostAwayRead, HostContract } from "../../lib/away";
import { formatLisbon, formatLisbonDay, lisbonField16, serverNow } from "../../lib/clock";
import { projectGroups, projectKey } from "../../lib/peers";
import type { ProjectGroup } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { awayBriefPath, awayBriefQuery, projectsQuery, queryKeyFor } from "../../lib/queries";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { createStore } from "../../lib/store";
import { ActionError, sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { useMachineRows, useNamedProjects } from "../board/peer-state";
import { ProjectDialog } from "./project-dialog";
import "./away.css";

export const MoonIcon = () => (
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

// The scope a host's contract takes: the shared limits and that host's own
// projects. Each host counts its own runs and checks its own quota.
function bodyOf(form: Form, projects: string[]) {
  const orders = form.orders.trim();
  const maxRuns = wholeNumber(form.maxRuns, 1, 10_000);
  const floor = wholeNumber(form.floor, 0, 100);
  return {
    until: form.until,
    projects,
    ...(orders ? { orders } : {}),
    ...(maxRuns ? { max_new_runs: maxRuns } : {}),
    ...(floor !== null && floor !== undefined ? { quota_floor_percent: floor } : {}),
    merge: mergeOf(form),
  };
}

function toggled(list: string[], value: string): string[] {
  return list.includes(value) ? list.filter((item) => item !== value) : [...list, value];
}

// One host's part of the away form: its label, whether the board may act on
// it, and its contract when one binds.
interface HostPart {
  // The peer's label; undefined for this host.
  host?: string;
  group: ProjectGroup<Project>;
  running: AwayBrief | null;
}

function partsOf(groups: ProjectGroup<Project>[], running: HostContract[]): HostPart[] {
  return groups.map((group) => {
    const host = group.peer ? (group.host ?? undefined) : undefined;
    const contract = running.find((item) => item.host === host);
    return { ...(host ? { host } : {}), group, running: contract?.brief ?? null };
  });
}

// One request of the form: a start, an edit or an end on one host.
interface Order {
  host?: string;
  method: "POST" | "PUT" | "DELETE";
  body: Record<string, unknown>;
}

// What the form sends, one request per host the board may act on. A host with
// projects chosen starts away mode, or edits its running contract. A host
// with none chosen whose contract binds ends it.
function ordersOf(form: Form, parts: HostPart[]): Order[] {
  return parts.flatMap(({ host, group, running }): Order[] => {
    if (!group.standing.canAct) return [];
    const target = host ? { host } : {};
    const chosen = group.projects
      .filter((project) => form.projects.includes(projectKey(project)))
      .map((project) => project.id);
    if (chosen.length > 0) {
      const body = { ...bodyOf(form, chosen), ...target };
      return [{ ...target, method: running ? "PUT" : "POST", body }];
    }
    return running ? [{ ...target, method: "DELETE", body: target }] : [];
  });
}

// Sends each order in turn. A peer's request takes the click's key with its
// label, so a retry after one host refused replays the hosts that applied.
async function sendOrders<T>(orders: Order[], key: string) {
  const answers: { host?: string; response: MutationResponse<T> }[] = [];
  for (const order of orders) {
    const idempotency_key = order.host ? `${key}:${order.host}` : key;
    // oxlint-disable-next-line no-await-in-loop -- one host at a time, in order
    const response = await sendMutation<T>(order.method, "/api/v1/away", {
      ...order.body,
      idempotency_key,
    });
    answers.push({ ...(order.host ? { host: order.host } : {}), response });
  }
  const last = answers.at(-1)?.response;
  if (!last) throw new ActionError(0, "invalid", "Choose at least one project.");
  return { answers, last };
}

function AwayDialog({
  parts,
  grouped,
  running,
  onClose,
}: {
  parts: HostPart[];
  grouped: boolean;
  running: HostContract[];
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const id = useId();
  const editing = running[0]?.brief ?? null;
  const [form, setForm] = useState<Form>(() =>
    editing
      ? { ...formOf(editing), projects: running.flatMap(contractKeys) }
      : freshForm(
          parts
            .filter((part) => part.group.standing.canAct)
            .flatMap((part) => part.group.projects.map(projectKey)),
        ),
  );
  const [shown, setShown] = useState(false);
  const until = useRef<HTMLInputElement>(null);
  const set = (change: Partial<Form>) => setForm((current) => ({ ...current, ...change }));
  const orders = ordersOf(form, parts);
  const problem =
    problemOf(form) ??
    (orders.some((order) => order.method !== "DELETE") ? null : "Choose at least one project.");
  // The orders are a pure function of the form, so their text keys a retry.
  const ordersText = JSON.stringify(orders);
  const hosts = parts.filter((part) => part.group.standing.canAct).length;

  const send = useCallback(
    async (key: string) => (await sendOrders(JSON.parse(ordersText) as Order[], key)).last,
    [ordersText],
  );
  const recorded = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: queryKeyFor(awayBriefPath) });
    onClose();
  }, [onClose, queryClient]);
  const action = useAction({
    binding: `away:${editing ? "edit" : "start"}:${ordersText}`,
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
          {parts.map(({ group, host }) => (
            <div
              key={host ?? ""}
              className="away-host"
              role={grouped ? "group" : undefined}
              aria-label={grouped ? (group.host ?? "This host") : undefined}
            >
              {grouped ? <span className="away-host-h">{group.host ?? "This host"}</span> : null}
              <div className="chips">
                {group.projects.map((project) => (
                  <label
                    key={projectKey(project)}
                    className="chip-check"
                    title={group.standing.reason ?? undefined}
                  >
                    <input
                      type="checkbox"
                      disabled={!group.standing.canAct}
                      checked={group.standing.canAct && form.projects.includes(projectKey(project))}
                      onChange={() =>
                        set({ projects: toggled(form.projects, projectKey(project)) })
                      }
                    />
                    <span
                      className="pdot"
                      style={
                        {
                          "--pc": projectColor(projectIndexOf(parts, project)),
                        } as CSSProperties
                      }
                    />
                    {project.name}
                  </label>
                ))}
              </div>
              {group.standing.canAct ? null : (
                <p className="pa-help away-out" data-testid="away-uncovered">
                  <b>Not covered: {group.standing.reason}.</b> These projects keep running with no
                  away limit, and their grants still merge.
                </p>
              )}
            </div>
          ))}
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
        {hosts > 1 ? (
          <p className="pa-help away-each">
            Each host keeps these limits on its own: it counts its own new runs and checks its own
            quota.
          </p>
        ) : null}
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
              <div className="chips">
                {MERGE_ACTIONS.map((name) => (
                  <label key={name} className="chip-check">
                    <input
                      type="checkbox"
                      checked={form.actions.includes(name)}
                      onChange={() => set({ actions: toggled(form.actions, name) })}
                    />
                    {name}
                  </label>
                ))}
                <label className="chip-check">
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

// Where a project sits in the form's whole list, for its dot's color.
function projectIndexOf(parts: HostPart[], project: Project): number {
  return parts
    .flatMap((part) => part.group.projects)
    .findIndex((item) => projectKey(item) === projectKey(project));
}

// "Alpha, Beta", by the project names the contract covers on its host.
function namesOf(contract: HostContract, projects: Project[]): string {
  return contract.brief.projects.map((id) => projectName(id, contract.host, projects)).join(", ");
}

function AwayPopover({
  running,
  unread,
  grouped,
  local,
  projects,
  onEdit,
  onEnd,
  ending,
  error,
}: {
  running: HostContract[];
  unread: HostAwayRead[];
  grouped: boolean;
  local: string | null;
  projects: Project[];
  onEdit: () => void;
  onEnd: () => void;
  ending: boolean;
  error: string | null;
}) {
  const until = earliestUntil(running);
  return (
    <div className="away-pop" role="dialog" aria-label="Away mode">
      <div className="away-pop-b">
        <b>Away until {formatLisbonDay(until)}</b>
        {running.map((contract) => {
          const { brief } = contract;
          const { spend } = brief;
          return (
            <dl
              key={contract.host ?? ""}
              className="props away-props"
              aria-label={grouped ? (contract.host ?? local ?? "This host") : undefined}
            >
              {grouped ? (
                <>
                  <dt>Host</dt>
                  <dd className="away-host-n">{contract.host ?? local ?? "This host"}</dd>
                </>
              ) : null}
              <dt>Projects</dt>
              <dd>{namesOf(contract, projects)}</dd>
              {grouped && brief.until_at !== until ? (
                <>
                  <dt>Until</dt>
                  <dd>{formatLisbonDay(brief.until_at)}</dd>
                </>
              ) : null}
              <dt>Runs</dt>
              <dd>
                {spend.max_new_runs === null
                  ? `${spend.new_runs} started`
                  : `${spend.new_runs} of ${spend.max_new_runs}`}
              </dd>
              <dt>Merges</dt>
              <dd>{mergeLabel(brief.merge)}</dd>
            </dl>
          );
        })}
        {unread.length > 0 ? (
          <dl className="props away-props">
            {unread.map((entry) => (
              <Unread key={entry.host} entry={entry} />
            ))}
          </dl>
        ) : null}
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

// A peer whose contract the board could not read: its status is not known.
function Unread({ entry }: { entry: HostAwayRead }) {
  return (
    <>
      <dt className="away-out">Not read</dt>
      <dd className="away-out" data-testid="away-unread">
        {entry.message}
      </dd>
    </>
  );
}

// The Away control in the header, left of Ask. Idle it is a button that opens
// the Start dialog. While away it is a status, and a popover holds the limits
// with Edit and End. End asks nothing: it only gives control back, and opens
// the brief.
// Counts the command palette's requests to open the control as a click on
// it does.
export const awayRequestStore = createStore(0);

export function AwayControl() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const response = useQuery(awayBriefQuery()).data;
  const projects = useNamedProjects(useQuery(projectsQuery()).data)?.projects ?? [];
  const machines = useMachineRows();
  const local = machines.find((machine) => machine.kind === "local")?.label ?? null;
  // Away covers each host's projects but its built-in one (UA-30).
  const groups = projectGroups(awayProjects(projects), machines, local);
  const grouped = groups.some((group) => group.peer);
  const running = runningOf(response);
  const unread = unreadHosts(response);
  const parts = partsOf(groups, running);
  const [dialog, setDialog] = useState<"start" | "edit" | null>(null);
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLSpanElement>(null);
  const until = earliestUntil(running);

  const closeDialog = () => setDialog(null);
  const isAway = running.length > 0;
  const canStart = parts.some((part) => part.group.standing.canAct && part.group.projects.length);
  useEffect(
    () =>
      awayRequestStore.subscribe(() => {
        if (isAway) setOpen(true);
        else if (canStart) setDialog("start");
      }),
    [isAway, canStart],
  );
  // End sends a DELETE to each host whose contract binds.
  const ends = JSON.stringify(
    running.map(({ host }): Order => ({
      ...(host ? { host } : {}),
      method: "DELETE",
      body: host ? { host } : {},
    })),
  );
  const end = useAction<{ brief: AwayBrief | null }>({
    binding: `away:end:${running.map(({ host, brief }) => `${host ?? ""}/${brief.approval_id}`).join(",")}`,
    send: async (key: string) => {
      const { answers, last } = await sendOrders<{ brief: AwayBrief | null }>(
        JSON.parse(ends) as Order[],
        key,
      );
      // Each ended brief shows at once, before the read comes back.
      queryClient.setQueryData<AwayBriefResponse>(queryKeyFor(awayBriefPath), (current) =>
        endedIn(current, answers),
      );
      return last;
    },
    onRecorded: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeyFor(awayBriefPath) });
      setOpen(false);
      void navigate({ to: "/inbox", search: { item: AWAY_BRIEF_ID } });
    },
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
      {until ? (
        <button
          type="button"
          className="btn away-pill"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={`Away until ${formatLisbonDay(until)}`}
          onClick={() => setOpen((current) => !current)}
        >
          <i className="ld live" />
          <span className="away-when">Away until {formatLisbon(until)}</span>
        </button>
      ) : (
        <button
          type="button"
          className="btn away-open"
          // On a phone, beside New card, only the moon shows.
          aria-label="Away"
          disabled={!canStart}
          onClick={() => setDialog("start")}
        >
          <MoonIcon />
          <span className="away-label">Away</span>
        </button>
      )}
      {isAway && open ? (
        <AwayPopover
          running={running}
          unread={unread}
          grouped={grouped}
          local={local}
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
          parts={parts}
          grouped={grouped}
          running={dialog === "edit" ? running : []}
          onClose={closeDialog}
        />
      ) : null}
    </span>
  );
}

// The away read with each ended contract in place of the running one.
function endedIn(
  current: AwayBriefResponse | undefined,
  answers: { host?: string; response: MutationResponse<{ brief: AwayBrief | null }> }[],
): AwayBriefResponse | undefined {
  if (!current) return current;
  const ended = (host: string | undefined) =>
    answers.find((answer) => answer.host === host)?.response.data.brief;
  return {
    brief: ended(undefined) ?? current.brief,
    hosts: (current.hosts ?? []).map((entry) => ({
      ...entry,
      brief: ended(entry.host) ?? entry.brief,
    })),
  };
}
