import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { projectRetireQuery } from "../../lib/queries";
import { onProjectGone } from "../../lib/sse";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { AgentList, FLEET_PROJECT, ProjectDialog, agentCount, readFailure } from "./project-dialog";

// The confirm step of a project archive (`project.retire`). With agents still
// running it asks whether to stop them, and never leaves them running unsaid.
function ConfirmArchive({ project, onClose }: { project: Project; onClose: () => void }) {
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
  const running = ready && agents.length > 0;
  // Only the host an agent runs on can stop it, so with one elsewhere the
  // archive here cannot offer the stop.
  const remote = agents.filter(({ local }) => !local);
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
              : "Stop them now, or archive and leave them running."}
          </p>
        </>
      ) : null}
      {refused ? (
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
              {stopping.pending ? "Stopping..." : `Stop ${agentCount(agents)} and archive`}
            </button>
          </>
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
  const [confirming, setConfirming] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const off = owner ? `${owner} owns this project: archive it there` : undefined;
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
        className="btn"
        disabled={off !== undefined}
        title={off}
        onClick={() => setConfirming(true)}
      >
        Archive
      </button>
      {confirming ? <ConfirmArchive project={project} onClose={close} /> : null}
    </>
  );
}
