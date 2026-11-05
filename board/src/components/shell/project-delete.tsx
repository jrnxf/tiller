import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";

import type { Project, ProjectDeletePreview, ProjectUnlanded } from "../../lib/api-types";
import { projectDeleteQuery } from "../../lib/queries";
import { onProjectGone } from "../../lib/sse";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { AgentList, FLEET_PROJECT, ProjectDialog, agentCount, readFailure } from "./project-dialog";

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
// first, the agents it stops before, and the work it leaves where it is.
function Facts({ preview }: { preview: ProjectDeletePreview }) {
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

// The confirm step of a project delete: what goes, the agents it stops first,
// and the id typed again.
function ConfirmDelete({ project, onClose }: { project: Project; onClose: () => void }) {
  const queryClient = useQueryClient();
  const preview = useQuery(projectDeleteQuery(project.id));
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
        <Facts preview={preview.data} />
      ) : failure ? null : (
        <p className="delete-note" role="status">
          Counting what goes...
        </p>
      )}
      {refused ? (
        <p className="refusal" role="alert">
          {refused}
        </p>
      ) : failure ? (
        <p className="refusal" role="alert">
          {failure}
        </p>
      ) : null}
      {refused && unblock ? (
        <div className="delete-next">
          <p>Next: {unblock.step}, then delete again.</p>
          {unblock.command ? <code>{unblock.command}</code> : null}
        </div>
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
  const opener = useRef<HTMLButtonElement>(null);
  const off = owner ? `${owner} owns this project: delete it there` : undefined;
  const close = useCallback(() => {
    setConfirming(false);
    opener.current?.focus();
  }, []);
  if (project.id === FLEET_PROJECT) return null;
  return (
    <>
      <button
        ref={opener}
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
