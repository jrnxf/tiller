import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { projectRetireQuery } from "../../lib/queries";
import { onProjectGone } from "../../lib/sse";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { ConfirmDelete } from "./project-delete";
import { AgentList, FLEET_PROJECT, ProjectDialog, agentCount, readFailure } from "./project-dialog";

// The confirm step of a project archive (`project.retire`). With agents still
// running it asks whether to stop them, and never leaves them running unsaid.
// An open card refuses the archive before any stop, so the agents a stop
// reaches are those left on closed cards, in practice the orchestrator; with
// one open it offers the delete instead, which stops them all.
function ConfirmArchive({
  project,
  onClose,
  onDelete,
}: {
  project: Project;
  onClose: () => void;
  onDelete: () => void;
}) {
  const queryClient = useQueryClient();
  const preview = useQuery(projectRetireQuery(project.id));

  const recorded = useCallback(() => {
    onClose();
    onProjectGone(queryClient, project.id);
  }, [onClose, project.id, queryClient]);
  const retire = useCallback(
    (stop_agents: boolean) => (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/projects/${project.id}/retire`, {
        stop_agents,
        idempotency_key,
      }),
    [project.id],
  );
  const stopping = useAction({
    binding: `${project.id}:retire:stop`,
    send: retire(true),
    onRecorded: recorded,
  });
  const keeping = useAction({
    binding: `${project.id}:retire:keep`,
    send: retire(false),
    onRecorded: recorded,
  });
  const pending = stopping.pending || keeping.pending;
  const error = stopping.error ?? keeping.error;
  // A refused stop may have stopped some agents first: read what is left.
  useEffect(() => {
    if (error) {
      void queryClient.invalidateQueries({ queryKey: projectRetireQuery(project.id).queryKey });
    }
  }, [error, project.id, queryClient]);

  const refused = preview.data?.refused ?? null;
  const agents = preview.data?.agents ?? [];
  const ready = preview.data !== undefined && refused === null;
  // An open card is the refusal a delete answers, since it cancels the card.
  const openCard = refused?.endsWith(" is open") === true;
  const running = ready && agents.length > 0;
  // Only the host an agent runs on can stop it, so with one elsewhere the
  // archive here cannot offer the stop.
  const remote = agents.filter(({ local }) => !local);
  // What a stop reaches, the orchestrator alone in practice.
  const lone = agents.length === 1 && agents[0]?.role === "orchestrator";
  const cancel = useRef<HTMLButtonElement>(null);
  const failure = readFailure(
    preview.error,
    "The project could not be read, so it cannot be archived yet.",
  );

  return (
    <ProjectDialog name="archive" pending={pending} onClose={onClose} initial={cancel}>
      <h2 id="archive-dialog-title">Archive {project.name}?</h2>
      <p id="archive-dialog-body">
        The project leaves the board and takes no new work. Its cards and history stay on this host,
        and its id stays taken.
      </p>
      {preview.data ? null : failure ? null : (
        <p className="delete-note" role="status">
          Checking the project...
        </p>
      )}
      {running ? (
        <>
          <div className="pdialog-card">
            <AgentList agents={agents} title="Still running" />
          </div>
          <p className="delete-note">
            {remote.length > 0
              ? `Only its own host can stop an agent, and ${remote.length} of these ${remote.length === 1 ? "runs" : "run"} elsewhere. Archiving here leaves them all running.`
              : lone
                ? "Stop the orchestrator now, or archive and leave it running."
                : "Stop them now, or archive and leave them running."}
          </p>
        </>
      ) : null}
      {openCard ? (
        <>
          <p className="refusal" role="alert">
            Archiving needs its open cards finished or cancelled first: {refused}.
          </p>
          <p className="delete-note">
            Or delete the project instead: that stops its agents, cancels its cards and removes it
            from this host.
          </p>
        </>
      ) : refused ? (
        <p className="refusal" role="alert">
          {refused}
        </p>
      ) : failure ? (
        <p className="refusal" role="alert">
          {failure}
        </p>
      ) : null}
      {error ? (
        <p className="refusal" role="alert">
          {error.message}
        </p>
      ) : null}
      <div className={running && remote.length === 0 ? "dialog-actions three" : "dialog-actions"}>
        <button ref={cancel} type="button" className="btn" disabled={pending} onClick={onClose}>
          Cancel
        </button>
        {running && remote.length > 0 ? (
          <button
            type="button"
            className="btn primary"
            disabled={pending}
            onClick={keeping.trigger}
          >
            {keeping.pending ? "Archiving..." : "Archive, keep running"}
          </button>
        ) : running ? (
          <>
            <button type="button" className="btn" disabled={pending} onClick={keeping.trigger}>
              {keeping.pending ? "Archiving..." : "Archive, keep running"}
            </button>
            <button
              type="button"
              className="btn primary"
              disabled={pending}
              onClick={stopping.trigger}
            >
              {stopping.pending
                ? "Stopping..."
                : lone
                  ? "Stop and archive"
                  : `Stop ${agentCount(agents)} and archive`}
            </button>
          </>
        ) : openCard ? (
          <button type="button" className="btn danger" onClick={onDelete}>
            Delete instead
          </button>
        ) : (
          <button
            type="button"
            className="btn primary"
            disabled={!ready || pending}
            onClick={keeping.trigger}
          >
            {keeping.pending ? "Archiving..." : "Archive project"}
          </button>
        )}
      </div>
    </ProjectDialog>
  );
}

// The archive control in a project's actions row: the plain one, beside the
// delete. A peer's project is archived on its own host.
export function ArchiveProject({
  project,
  owner,
}: {
  project: Project;
  owner: string | undefined;
}) {
  // The archive's confirm step, or the delete it offers in its place.
  const [confirming, setConfirming] = useState<"archive" | "delete" | null>(null);
  const off = owner ? `${owner} owns this project: archive it there` : undefined;
  // The dialog gives focus back to the button that opened it.
  const close = useCallback(() => {
    setConfirming(null);
  }, []);
  const toDelete = useCallback(() => setConfirming("delete"), []);
  if (project.id === FLEET_PROJECT) return null;
  return (
    <>
      <button
        type="button"
        className="btn"
        disabled={off !== undefined}
        title={off}
        onClick={() => setConfirming("archive")}
      >
        Archive
      </button>
      {confirming === "archive" ? (
        <ConfirmArchive project={project} onClose={close} onDelete={toDelete} />
      ) : confirming === "delete" ? (
        <ConfirmDelete project={project} onClose={close} />
      ) : null}
    </>
  );
}
