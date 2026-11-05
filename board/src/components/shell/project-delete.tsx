import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";

import type { Project, ProjectDeletePreview } from "../../lib/api-types";
import { ReadError, projectDeleteQuery } from "../../lib/queries";
import { onProjectDeleted } from "../../lib/sse";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";

const FLEET_PROJECT = "fleet";

// "memory_revisions" reads as "memory revisions".
function tableName(table: string): string {
  return table.replaceAll("_", " ");
}

// The tables the delete removes rows from, largest first.
function Counts({ preview }: { preview: ProjectDeletePreview }) {
  const rows = Object.entries(preview.removed).filter(([, count]) => count > 0);
  rows.sort(([a, x], [b, y]) => y - x || a.localeCompare(b));
  const unlinked = Object.values(preview.unlinked).reduce((sum, count) => sum + count, 0);
  return (
    <>
      <ul className="delete-counts" aria-label="Rows removed">
        {rows.map(([table, count]) => (
          <li key={table}>
            <span>{tableName(table)}</span>
            <b>{count}</b>
          </li>
        ))}
      </ul>
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

// The confirm step of a project delete: what goes, and the id typed again. It
// is a modal on the page itself, like the stage switch's.
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
    onProjectDeleted(queryClient, project.id);
  }, [onClose, project.id, queryClient]);
  const action = useAction({ binding: `${project.id}:delete`, send, onRecorded: recorded });

  const refused = preview.data?.refused ?? null;
  const unblock = preview.data?.unblock ?? null;
  const ready = preview.data !== undefined && refused === null;
  const armed = ready && typed.trim() === project.id && !action.pending;

  const dialog = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancel.current?.focus();
  }, []);
  useEffect(() => {
    // Once the counts are in, the next thing to do is type the id.
    if (ready) input.current?.focus();
  }, [ready]);

  const readFailure =
    preview.error instanceof ReadError
      ? (preview.error.reason ?? preview.error.message)
      : preview.error
        ? "The counts could not be read, so nothing can be deleted yet."
        : null;

  return createPortal(
    <div
      className="dialog-backdrop"
      onMouseDown={() => {
        if (!action.pending) onClose();
      }}
    >
      <div
        ref={dialog}
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="delete-dialog-title"
        aria-describedby="delete-dialog-body"
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!action.pending) onClose();
          } else if (event.key === "Tab") {
            // Keep focus on the dialog's own controls that can take it.
            const stops = [
              ...(dialog.current?.querySelectorAll<HTMLElement>("input, button") ?? []),
            ].filter((element) => !(element as HTMLButtonElement).disabled);
            const [first, last] = [stops[0], stops.at(-1)];
            if (!first) {
              // Everything is disabled while the delete is sent: focus stays.
              event.preventDefault();
            } else if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last?.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first?.focus();
            }
          }
        }}
      >
        <h2 id="delete-dialog-title">Delete {project.name}?</h2>
        <p id="delete-dialog-body">
          This removes the project and everything recorded for it on this host, its event history
          included. It cannot be undone. The checkout, its worktrees and the GitHub repository are
          left as they are.
        </p>
        {preview.data ? (
          <Counts preview={preview.data} />
        ) : readFailure ? null : (
          <p className="delete-note" role="status">
            Counting what goes...
          </p>
        )}
        {refused ? (
          <p className="refusal" role="alert">
            {refused}
          </p>
        ) : readFailure ? (
          <p className="refusal" role="alert">
            {readFailure}
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
            {action.pending ? "Deleting..." : "Delete project"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

// The delete control under a project's stage facts. A peer's project is
// deleted on its own host: the board host does not forward it (protocol.md
// section 7.4). `fleet` is never deleted, so it has no control.
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
        className="btn ghost project-delete"
        disabled={off !== undefined}
        title={off}
        onClick={() => setConfirming(true)}
      >
        Delete project
      </button>
      {confirming ? <ConfirmDelete project={project} onClose={close} /> : null}
    </>
  );
}
