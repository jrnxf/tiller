import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";

import type {
  DeleteUnblock,
  Project,
  ProjectAgent,
  ProjectDeletePreview,
  ProjectUnlanded,
} from "../../lib/api-types";
import { projectDeleteQuery } from "../../lib/queries";
import { onProjectGone } from "../../lib/sse";
import { sendMutation } from "../attention/api";
import { CancelDialog } from "../attention/card-lifecycle";
import { useAction } from "../attention/use-action";
import { RunDialogs } from "../board/run-menu";
import { ClockIcon } from "../memory/icons";
import { CheckIcon, ServerIcon } from "./icons";
import {
  AgentList,
  FLEET_PROJECT,
  ProjectDialog,
  agentCount,
  agentName,
  readFailure,
} from "./project-dialog";

// How often a blocked delete reads its counts again.
const BLOCKED_POLL_MS = 5_000;

// "memory_revisions" reads as "memory revisions".
function tableName(table: string): string {
  return table.replaceAll("_", " ");
}

// "3 commits not landed, uncommitted changes". A worktree not checked yet
// says so rather than look clean.
function unlandedDetail(item: ProjectUnlanded): string {
  const { unlanded_commits: commits, dirty } = item;
  if (commits === null && dirty === null) return "not checked yet";
  const parts = [];
  if (commits === null) parts.push("commits not checked yet");
  else if (commits > 0) {
    parts.push(commits === 1 ? "1 commit not landed" : `${commits} commits not landed`);
  }
  if (dirty === null) parts.push("changes not checked yet");
  else if (dirty) parts.push("uncommitted changes");
  return parts.join(", ") || "not landed";
}

// What the delete does, in one card: the tables it removes rows from, largest
// first, the agents it stops before, the work it leaves where it is, and what
// blocks it (`children`).
function Facts({ preview, children }: { preview: ProjectDeletePreview; children?: ReactNode }) {
  const rows = Object.entries(preview.removed).filter(([, count]) => count > 0);
  rows.sort(([a, x], [b, y]) => y - x || a.localeCompare(b));
  const unlinked = Object.values(preview.unlinked).reduce((sum, count) => sum + count, 0);
  return (
    <>
      <div className="pdialog-card">
        <ul className="delete-counts" aria-label="Rows removed">
          {rows.map(([table, count]) => (
            <li key={table}>
              <span>{tableName(table)}</span>
              <b>{count}</b>
            </li>
          ))}
        </ul>
        {preview.agents.length > 0 ? (
          <AgentList agents={preview.agents} title="Stopped first" />
        ) : null}
        {preview.unlanded.length > 0 ? (
          <section className="pdialog-section" aria-label="Not landed">
            <h3>Not landed, left on disk</h3>
            <ul>
              {preview.unlanded.map((item) => (
                <li key={item.path} className="stacked">
                  <code>{item.path}</code>
                  <small>
                    {item.card ? `${item.card}: ` : ""}
                    {unlandedDetail(item)}
                  </small>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {preview.open_prs.length > 0 ? (
          <section className="pdialog-section" aria-label="Open pull requests">
            <h3>Open PRs, left on GitHub</h3>
            <ul>
              {preview.open_prs.map((pr) => (
                <li key={pr.url}>
                  <a href={pr.url} target="_blank" rel="noreferrer">
                    #{pr.number}
                  </a>
                  <small>{pr.card}</small>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {children}
      </div>
      {unlinked > 0 ? (
        <p className="delete-note">
          {unlinked === 1
            ? "1 link with another project's records is cleared."
            : `${unlinked} links with other projects' records are cleared.`}
        </p>
      ) : null}
    </>
  );
}

// A blocker the owner cleared while the dialog was open: what it was, what
// happened to it, the word on its right, and the preview read it was cleared
// against (`seen`), so a later read that still reports it shows it again.
interface Cleared {
  key: string;
  title: string;
  meta: string;
  outcome: "Cancelled" | "Stopped" | "Done";
  seen: number;
}

// `list` with `row` added: in place of a row for the same blocker when
// `replace`, else only when there is none yet.
function withCleared(list: Cleared[], row: Cleared, replace: boolean): Cleared[] {
  const has = list.some((item) => item.key === row.key);
  if (has && !replace) return list;
  return [...list.filter((item) => item.key !== row.key), row];
}

// The fact without its "; end it first" tail: the row's button says that.
function blockTitle(fact: string): string {
  const head = fact.split("; ")[0] ?? fact;
  return head.charAt(0).toUpperCase() + head.slice(1);
}

// What clears a blocker, said the way the row says it, and what the row
// says once it went away without the board's own action.
function blockKind(
  unblock: DeleteUnblock,
  agents: ProjectAgent[],
): { kind: "cancel" | "stop" | "host" | "wait"; agent?: ProjectAgent; gone: string } {
  const { action } = unblock;
  if (action?.id === "card.cancel") return { kind: "cancel", gone: "Its card was cancelled." };
  if (action?.id === "agent.stop") {
    const agent = agents.find((item) => item.session === action.target);
    return agent?.local
      ? { kind: "stop", agent, gone: "Its agent was stopped." }
      : { kind: "host", gone: "Stopped on its host." };
  }
  return { kind: "wait", gone: "It finished." };
}

// One blocker of the delete with the action that clears it on its right:
// Cancel card and Stop agent open the card's and the Run menu's own confirm
// dialogs. An agent on another host and a wait have no action here.
function Blocker({
  project,
  fact,
  unblock,
  agents,
  asking,
  setAsking,
  onCleared,
}: {
  project: Project;
  fact: string;
  unblock: DeleteUnblock;
  agents: ProjectAgent[];
  // Whether the row's confirm is open: the dialog holds it, so a read that
  // lands meanwhile does not drop the confirm.
  asking: boolean;
  setAsking: (asking: boolean) => void;
  onCleared: (row: Omit<Cleared, "seen">) => void;
}) {
  const close = useCallback(() => setAsking(false), [setAsking]);
  const title = blockTitle(fact);
  const { kind, agent } = blockKind(unblock, agents);
  const target = unblock.action?.target ?? "";
  const cancelled = useCallback(
    (reason: string) =>
      onCleared({ key: fact, title, meta: `Card cancelled: ${reason}`, outcome: "Cancelled" }),
    [fact, onCleared, title],
  );
  const stopped = useCallback(
    (warning: string | null) =>
      onCleared({
        key: fact,
        title,
        meta: warning ?? "Its pane is closed and its session ended.",
        outcome: "Stopped",
      }),
    [fact, onCleared, title],
  );
  const step = unblock.step.charAt(0).toUpperCase() + unblock.step.slice(1);
  return (
    <li>
      <span className="t">{title}</span>
      {kind === "cancel" ? (
        <>
          <span className="m">A run ends when its card is cancelled.</span>
          <button type="button" className="btn sm" onClick={() => setAsking(true)}>
            Cancel card...
          </button>
          {asking ? (
            <CancelDialog
              card={{ id: target, display_id: target }}
              reason={`deleting project ${project.id}`}
              onClose={close}
              onCancelled={cancelled}
            />
          ) : null}
        </>
      ) : kind === "stop" && agent ? (
        <>
          <span className="m">Stopping it closes its pane and ends its session.</span>
          <button type="button" className="btn sm" onClick={() => setAsking(true)}>
            Stop agent...
          </button>
          <RunDialogs
            target={{
              sessionId: target,
              name: agentName(agent),
              orchestrator: agent.card === null,
              state: "running",
              runOpen: agent.run !== null,
            }}
            dialog={asking ? "stop" : null}
            onClose={close}
            onStopped={stopped}
          />
        </>
      ) : kind === "host" ? (
        <>
          <span className="m">Only its host can stop it. This list updates by itself.</span>
          <span className="r wait">
            <ServerIcon />
            On its host
          </span>
        </>
      ) : (
        <>
          <span className="m">{step}. This list updates by itself.</span>
          <span className="r wait">
            <ClockIcon />
            Waiting
          </span>
        </>
      )}
    </li>
  );
}

// The confirm step of a project delete: what goes, the agents it stops first,
// what blocks it with the action that clears each, and the id typed again.
export function ConfirmDelete({ project, onClose }: { project: Project; onClose: () => void }) {
  const queryClient = useQueryClient();
  // While a blocker has a step past it, the counts are read again now and
  // then: a wait ends, or an agent on another host stops, without the board.
  // A refusal with no step (a stray reference) does not clear by itself, and
  // no read runs while a blocker's confirm is open.
  const [asking, setAsking] = useState(false);
  const preview = useQuery({
    ...projectDeleteQuery(project.id),
    refetchInterval: (query) => (query.state.data?.unblock && !asking ? BLOCKED_POLL_MS : false),
  });
  const [typed, setTyped] = useState("");
  const inputId = useId();

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/projects/${project.id}/delete`, {
        confirm: project.id,
        idempotency_key,
      }),
    [project.id],
  );
  const recorded = useCallback(() => {
    onClose();
    onProjectGone(queryClient, project.id);
  }, [onClose, project.id, queryClient]);
  const action = useAction({ binding: `${project.id}:delete`, send, onRecorded: recorded });
  // A refused delete may have stopped some agents first: read what is left.
  useEffect(() => {
    if (action.error) {
      void queryClient.invalidateQueries({ queryKey: projectDeleteQuery(project.id).queryKey });
    }
  }, [action.error, project.id, queryClient]);

  const refused = preview.data?.refused ?? null;
  const unblock = preview.data?.unblock ?? null;
  const agents = preview.data?.agents ?? [];
  const ready = preview.data !== undefined && refused === null;

  // The blockers cleared since the dialog opened: by the row's own action,
  // or gone by themselves between two reads.
  const [cleared, setCleared] = useState<Cleared[]>([]);
  const readAt = preview.dataUpdatedAt;
  const onCleared = useCallback(
    (row: Omit<Cleared, "seen">) => {
      setCleared((list) => withCleared(list, { ...row, seen: readAt }, true));
      void queryClient.invalidateQueries({ queryKey: projectDeleteQuery(project.id).queryKey });
    },
    [project.id, queryClient, readAt],
  );
  const blocking = refused && unblock ? { fact: refused, unblock } : null;
  const shown = useRef<{ key: string; gone: string } | null>(null);
  useEffect(() => {
    const data = preview.data;
    if (data === undefined) return;
    const before = shown.current;
    shown.current =
      data.refused && data.unblock
        ? { key: data.refused, gone: blockKind(data.unblock, data.agents).gone }
        : null;
    if (before && before.key !== shown.current?.key) {
      const row: Cleared = {
        key: before.key,
        title: blockTitle(before.key),
        meta: before.gone,
        outcome: "Done",
        seen: preview.dataUpdatedAt,
      };
      setCleared((list) => withCleared(list, row, false));
    }
  }, [preview.data, preview.dataUpdatedAt]);
  // A cleared blocker that a later read still reports is live again: its
  // row goes back to its action (a stop that left the pane open, say).
  const shownCleared = cleared.filter((row) => !(blocking?.fact === row.key && readAt > row.seen));
  const current =
    blocking && !shownCleared.some((item) => item.key === blocking.fact) ? blocking : null;
  const armed = ready && typed.trim() === project.id && !action.pending;

  const input = useRef<HTMLInputElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    // Once the counts are in, the next thing to do is type the id.
    if (ready) input.current?.focus();
  }, [ready]);

  const failure = readFailure(
    preview.error,
    "The counts could not be read, so nothing can be deleted yet.",
  );

  return (
    <ProjectDialog name="delete" pending={action.pending} onClose={onClose} initial={cancel}>
      <h2 id="delete-dialog-title">Delete {project.name}?</h2>
      <p id="delete-dialog-body">
        {agents.length > 0
          ? "This stops the project's running agents, then removes"
          : "This removes"}{" "}
        the project and everything recorded for it on this host, its event history included. It
        cannot be undone. The checkout, its worktrees and the GitHub repository are left as they
        are.
      </p>
      {preview.data ? (
        <Facts preview={preview.data}>
          {current || shownCleared.length > 0 ? (
            <section className="pdialog-section" aria-label="Blocks the delete">
              <h3>Blocks the delete</h3>
              <ul className="delete-blocks">
                {shownCleared.map((row) => (
                  <li key={row.key}>
                    <span className="t">{row.title}</span>
                    <span className="m">{row.meta}</span>
                    <span className="r">
                      <CheckIcon />
                      {row.outcome}
                    </span>
                  </li>
                ))}
                {current ? (
                  <Blocker
                    key={current.fact}
                    project={project}
                    fact={current.fact}
                    unblock={current.unblock}
                    agents={agents}
                    asking={asking}
                    setAsking={setAsking}
                    onCleared={onCleared}
                  />
                ) : null}
              </ul>
            </section>
          ) : null}
        </Facts>
      ) : failure ? null : (
        <p className="delete-note" role="status">
          Counting what goes...
        </p>
      )}
      {/* The state the dialog opens in, not a failed press: a status. */}
      {current ? (
        <p className="refusal" role="status">
          Not deleted: clear what blocks it first.
        </p>
      ) : refused && !blocking ? (
        <p className="refusal" role="alert">
          {refused}
        </p>
      ) : failure ? (
        <p className="refusal" role="alert">
          {failure}
        </p>
      ) : null}
      {ready ? (
        <div className="delete-confirm">
          <label htmlFor={inputId}>
            Type <code>{project.id}</code> to confirm
          </label>
          <input
            ref={input}
            id={inputId}
            className="note-in"
            autoComplete="off"
            spellCheck={false}
            value={typed}
            disabled={action.pending}
            onChange={(event) => setTyped(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && armed) action.trigger();
            }}
          />
        </div>
      ) : null}
      {action.error ? (
        <p className="refusal" role="alert">
          {action.error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button
          ref={cancel}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Cancel
        </button>
        <button type="button" className="btn danger" disabled={!armed} onClick={action.trigger}>
          {action.pending
            ? agents.length > 0
              ? "Stopping and deleting..."
              : "Deleting..."
            : agents.length > 0
              ? `Stop ${agentCount(agents)} and delete`
              : "Delete project"}
        </button>
      </div>
    </ProjectDialog>
  );
}

// The delete control in a project's actions row. A peer's project is deleted
// on its own host: the board host does not forward it (protocol.md section
// 7.4). `fleet` is never deleted, so it has no control.
export function DeleteProject({ project, owner }: { project: Project; owner: string | undefined }) {
  const [confirming, setConfirming] = useState(false);
  const off = owner ? `${owner} owns this project: delete it there` : undefined;
  // The dialog gives focus back to the button that opened it.
  const close = useCallback(() => {
    setConfirming(false);
  }, []);
  if (project.id === FLEET_PROJECT) return null;
  return (
    <>
      <button
        type="button"
        className="btn project-delete"
        disabled={off !== undefined}
        title={off}
        onClick={() => setConfirming(true)}
      >
        Delete
      </button>
      {confirming ? <ConfirmDelete project={project} onClose={close} /> : null}
    </>
  );
}
