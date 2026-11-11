import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useId, useRef, useState } from "react";

import type { BoardRow, Machine, MutationResponse, Project } from "../../lib/api-types";
import { HOST_PROJECT, hostField, hostName, isPeerRow, ownerOf } from "../../lib/peers";
import type { Standing } from "../../lib/peers";
import { cardQuery } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { ProjectDialog } from "../shell/project-dialog";
import { Select } from "../ui/select";
import "../shell/project-stage.css";
import { bugProjectOf, cardName, machineOf, moveTargets } from "./bugs";
import type { MoveTarget } from "./bugs";
import { RetryIcon, RouteIcon, WarnIcon } from "./icons";
import { moveBug } from "./move";
import type { MoveResult } from "./move";
import { useMachineRows } from "./peer-state";
import { showToast } from "./toast";

// "Close as not a bug" closes the card with this outcome and reason.
const NOT_A_BUG = { outcome: "cancelled", reason: "Not a Tiller bug" } as const;

const PICK_IS_LOCAL =
  "A bug project is picked on the host that owns it: run tiller host edit --bug-project there.";

// The projects a host can pick to take Tiller bugs: its own, never `fleet`.
function pickable(projects: Project[]): Project[] {
  return projects.filter((project) => !isPeerRow(project) && project.id !== HOST_PROJECT);
}

// "Moved to host-a · tiller-21" once both steps landed.
function movedToast(result: Extract<MoveResult, { state: "moved" }>, source: string) {
  showToast("ok", `Moved to ${result.made}`, `Closed ${source} as superseded by it.`);
}

// What a Move's partial state says: the new card exists, the old one is
// still open, and the retry closes it without filing a second card.
function PartialMove({
  made,
  source,
  error,
  host,
}: {
  made: string;
  source: string;
  error: ActionError;
  host: string | undefined;
}) {
  return (
    <div className="move-partial" role="alert" data-testid="move-partial">
      <p>
        <b>
          Made {made}, but could not close {source}.
        </b>{" "}
        Both cards are open. Press Move again: it closes {source} and does not file a second card.
      </p>
      <ActionFailure error={error} host={host} />
    </div>
  );
}

// The fallback a Tiller bug in `fleet` gets on its card page (mocks,
// "Fallback card"): no orchestrator triages it there, so the owner moves it to
// a host that takes Tiller bugs, closes it, or picks a bug project.
export function BugTriage({
  row,
  body,
  projects,
  standing,
}: {
  row: BoardRow;
  body: string | null;
  projects: Project[];
  standing: Standing;
}) {
  const machines = useMachineRows();
  const owner = machineOf(row, machines);
  const hostLabel = owner?.label ?? row.host ?? null;
  const host = hostLabel ?? "This host";
  const source = cardName(row, hostLabel);
  const targets = moveTargets(machines, projects);
  const ownBugProject = bugProjectOf(owner, projects);
  const [moving, setMoving] = useState<string | null>(null);
  const [result, setResult] = useState<{ target: string; result: MoveResult } | null>(null);
  const [picking, setPicking] = useState(false);
  const pickButton = useRef<HTMLButtonElement>(null);

  const move = (target: MoveTarget) => {
    if (moving) return;
    setMoving(target.label);
    setResult(null);
    void moveBug({ row, body, hostLabel }, target).then((outcome) => {
      setMoving(null);
      setResult({ target: target.label, result: outcome });
      if (outcome.state === "moved") movedToast(outcome, source);
    });
  };

  const rowId = row.id;
  const rowHost = hostField(row).host;
  const sendClose = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/cards/${rowId}/close`, {
        ...NOT_A_BUG,
        ...(rowHost ? { host: rowHost } : {}),
        idempotency_key,
      }),
    [rowHost, rowId],
  );
  const close = useAction({
    binding: `not-a-bug:${row.id}:${row.rev}`,
    send: sendClose,
    onRecorded: () => showToast("ok", `Closed ${source}`, "Closed as not a Tiller bug."),
  });

  const busy = moving !== null || close.pending;
  const off = standing.canAct ? null : standing.reason;
  const local = owner?.kind === "local";
  const preview = targets[0];
  const outcome = result?.result;

  return (
    <div className="callout" data-testid="bug-triage">
      <div className="callout-h">
        <WarnIcon />
        Nobody triages this card
      </div>
      <p>
        {ownBugProject
          ? `${host} files Tiller bugs into ${ownBugProject.name} now, but this card waits in ${hostName(hostLabel)} from before. Move it there, or close it.`
          : `${host} has no project that takes Tiller bugs, so this card waits for you. Move it to a host that has one, or close it.`}
      </p>
      <div className="acts">
        {targets.map((target) => (
          <button
            key={target.label}
            type="button"
            className="btn primary"
            disabled={busy || off !== null}
            title={off ?? `Move this card to ${target.label}`}
            onClick={() => move(target)}
          >
            {outcome?.state === "close-failed" && result?.target === target.label ? (
              <RetryIcon />
            ) : (
              <RouteIcon />
            )}
            {moving === target.label ? "Moving..." : `Move to ${target.label}`}
          </button>
        ))}
        <button
          type="button"
          className="btn"
          disabled={busy || close.recorded || off !== null}
          title={off ?? "Close this card: it is not a Tiller bug"}
          onClick={close.trigger}
        >
          {close.pending ? "Closing..." : "Close as not a bug"}
        </button>
        <button
          ref={pickButton}
          type="button"
          className="btn"
          disabled={busy || !local}
          title={local ? `Pick the project that takes Tiller bugs on ${host}` : PICK_IS_LOCAL}
          onClick={() => setPicking(true)}
        >
          Pick a bug project for {host}
        </button>
      </div>
      {outcome?.state === "close-failed" ? (
        <PartialMove
          made={outcome.made}
          source={source}
          error={outcome.error}
          host={ownerOf(row)}
        />
      ) : null}
      {outcome?.state === "create-failed" ? (
        <ActionFailure
          error={outcome.error}
          host={targets.find((t) => t.label === result?.target)?.machine.label}
        />
      ) : null}
      <ActionFailure error={close.error} host={ownerOf(row)} />
      <div className="preview">
        <div className="preview-l">What Move does</div>
        {preview
          ? `Makes a ${preview.project.name} card on ${preview.machine.label} with this title, text and a link back. Then closes ${source} as superseded by it.${preview.machine.label === hostLabel ? "" : ` Read the text first: it leaves ${host}.`}`
          : "No host takes Tiller bugs yet, so there is nowhere to move this card. Pick a bug project first."}
      </div>
      {picking && owner ? (
        <PickBugProjectDialog
          machine={owner}
          projects={pickable(projects)}
          onClose={() => {
            setPicking(false);
            pickButton.current?.focus();
          }}
        />
      ) : null}
    </div>
  );
}

// "Pick a bug project": which of this host's projects takes Tiller bugs, or
// none. The pick is this host's own setting, so it is never forwarded.
// What `POST /api/v1/bug-project` returns: this host's record after the
// edit, not its Machines row (protocol.md 7.4).
type PickedHost = { bug_project_id?: string | null };

function PickBugProjectDialog({
  machine,
  projects,
  onClose,
}: {
  machine: Machine;
  projects: Project[];
  onClose: () => void;
}) {
  const [value, setValue] = useState(machine.bug_project ?? "");
  const field = useId();
  const first = useRef<HTMLButtonElement>(null);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<PickedHost>("POST", "/api/v1/bug-project", {
        project: value || null,
        rev: machine.rev,
        idempotency_key,
      }),
    [machine.rev, value],
  );
  const recorded = useCallback(
    ({ data }: MutationResponse<PickedHost>) => {
      onClose();
      const picked = projects.find((project) => project.id === data?.bug_project_id);
      showToast(
        "ok",
        picked
          ? `${picked.name} takes Tiller bugs on ${machine.label}`
          : `No project takes Tiller bugs on ${machine.label}`,
        picked
          ? `New Tiller bugs go there. Move the ones that wait in ${hostName(machine.label)}.`
          : null,
      );
    },
    [machine.label, onClose, projects],
  );
  const action = useAction({
    binding: `bug-project:${machine.id}:${machine.rev}:${value}`,
    send,
    onRecorded: recorded,
  });
  const unchanged = value === (machine.bug_project ?? "");

  return (
    <ProjectDialog name="bug-project" pending={action.pending} onClose={onClose} initial={first}>
      <h2 id="bug-project-dialog-title">Pick a bug project for {machine.label}</h2>
      <p id="bug-project-dialog-body">
        <code>tiller bug file</code> files Tiller bugs into this project, and its orchestrator
        triages them. With none, they wait in {hostName(machine.label)} for you.
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!unchanged && !action.pending) action.trigger();
        }}
      >
        <div className="form-fields">
          <label htmlFor={`${field}-project`}>Project</label>
          <Select
            id={`${field}-project`}
            className="form-select"
            value={value}
            disabled={action.pending}
            onValueChange={setValue}
            options={[
              { value: "", label: "None" },
              ...projects.map((project) => ({ value: project.id, label: project.name })),
            ]}
          />
        </div>
        <ActionFailure error={action.error} host={undefined} />
        <div className="dialog-actions">
          <button
            ref={first}
            type="button"
            className="btn"
            disabled={action.pending}
            onClick={onClose}
          >
            Cancel
          </button>
          <button type="submit" className="btn primary" disabled={unchanged || action.pending}>
            {action.pending ? "Saving..." : "Save"}
          </button>
        </div>
      </form>
    </ProjectDialog>
  );
}

interface Moved {
  row: BoardRow;
  result: MoveResult | null;
}

// The lane's "Move N to <project>": once a host takes Tiller bugs, the ones
// that wait in its `fleet` from before move there, each as a card page Move.
export function MoveBugsDialog({
  rows,
  target,
  hostLabel,
  onClose,
}: {
  rows: BoardRow[];
  target: MoveTarget;
  hostLabel: string | null;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const [moved, setMoved] = useState<Moved[]>(() => rows.map((row) => ({ row, result: null })));
  const [pending, setPending] = useState(false);
  const first = useRef<HTMLButtonElement>(null);
  const left = moved.filter((entry) => entry.result?.state !== "moved");

  const run = async () => {
    setPending(true);
    const next = [...moved];
    for (const [index, entry] of next.entries()) {
      if (entry.result?.state === "moved") continue;
      let body: string | null = null;
      try {
        const detail = await client.fetchQuery(cardQuery(entry.row.id, ownerOf(entry.row)));
        body = detail.card.body ?? null;
      } catch {
        next[index] = {
          row: entry.row,
          result: {
            state: "create-failed",
            error: new ActionError(
              0,
              "unavailable",
              "Could not read its text, so it did not move.",
            ),
          },
        };
        continue;
      }
      next[index] = {
        row: entry.row,
        result: await moveBug({ row: entry.row, body, hostLabel }, target),
      };
      setMoved([...next]);
    }
    setMoved(next);
    setPending(false);
    if (next.every((entry) => entry.result?.state === "moved")) {
      onClose();
      showToast(
        "ok",
        `Moved ${next.length} Tiller bug${next.length === 1 ? "" : "s"} to ${target.label}`,
        "Each one closed here as superseded by its new card.",
      );
    }
  };

  return (
    <ProjectDialog name="move-bugs" pending={pending} onClose={onClose} initial={first}>
      <h2 id="move-bugs-dialog-title">Move Tiller bugs to {target.label}</h2>
      <p id="move-bugs-dialog-body">
        Each card becomes a {target.project.name} card with its title, text and a link back, then
        closes here as superseded by it. Its orchestrator triages them from there.
      </p>
      <section className="pdialog-section" aria-label="Cards to move">
        <h3>
          {rows.length} card{rows.length === 1 ? "" : "s"}
        </h3>
        <ul className="move-list">
          {moved.map(({ row, result }) => (
            <li key={row.id}>
              <span>
                <b>{cardName(row, hostLabel)}</b> {row.title}
              </span>
              {result?.state === "moved" ? (
                <span className="lab sm ok">Moved to {result.made}</span>
              ) : result?.state === "close-failed" ? (
                <span className="lab sm yel" title={result.error.message}>
                  Made {result.made}, still open here
                </span>
              ) : result?.state === "create-failed" ? (
                <span className="lab sm bad" title={result.error.message}>
                  Not moved
                </span>
              ) : null}
            </li>
          ))}
        </ul>
      </section>
      {left.some((entry) => entry.result) ? (
        <p className="refusal" role="alert">
          {left.length} card{left.length === 1 ? "" : "s"} did not move all the way. Press Retry:
          each step carries the same key, so no card is filed twice.
        </p>
      ) : null}
      <div className="dialog-actions">
        <button ref={first} type="button" className="btn" disabled={pending} onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={pending || left.length === 0}
          onClick={() => void run()}
        >
          {pending
            ? "Moving..."
            : left.some((entry) => entry.result)
              ? "Retry"
              : `Move ${left.length}`}
        </button>
      </div>
    </ProjectDialog>
  );
}
