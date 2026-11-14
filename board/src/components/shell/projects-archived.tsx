import { useQueries } from "@tanstack/react-query";
import { useCallback, useId, useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { isPeerRow, peerMachines, projectKey, standingOf } from "../../lib/peers";
import { retiredProjectsQuery } from "../../lib/queries";
import { formatWhen } from "../attention/format";
import { HostChip } from "../board/host-chip";
import { useMachineRows } from "../board/peer-state";
import { ChevronIcon, TrashIcon } from "./icons";
import { ConfirmDelete } from "./project-delete";

function ArchivedRow({ project }: { project: Project }) {
  const [confirming, setConfirming] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => {
    setConfirming(false);
    opener.current?.focus();
  }, []);
  return (
    <div className="prow" data-off="true">
      <span className="pbtn" title={`Archived ${formatWhen(project.retired_at ?? null)}`}>
        <span className="pdot" />
        <span className="nm">{project.name}</span>
        {isPeerRow(project) ? (
          <span className="pchips">
            <HostChip entity={project} />
          </span>
        ) : null}
      </span>
      <span className="pacts">
        <button
          ref={opener}
          type="button"
          className="eye delete"
          title={`Delete ${project.name}${isPeerRow(project) ? ` on ${project.host}` : ""}`}
          aria-label={`Delete ${project.name}`}
          onClick={() => setConfirming(true)}
        >
          <TrashIcon />
        </button>
      </span>
      {confirming ? <ConfirmDelete project={project} onClose={close} /> : null}
    </div>
  );
}

// The projects archived on this host, and on each peer the board can act on,
// folded under the live ones. Archiving keeps a project's history; its delete
// is the one thing left to do with it. A peer's archive is read from the peer,
// and the daemon tags its rows with the peer as it tags the live ones.
export function ArchivedProjects() {
  const machines = useMachineRows();
  const peers = peerMachines(machines)
    .filter((machine) => standingOf({ host: machine.label, link: machine.link }, machines).canAct)
    .map((machine) => machine.label);
  const hosts = [undefined, ...peers];
  const archived = useQueries({
    queries: hosts.map((host) => retiredProjectsQuery(host)),
    combine: (results) => results.flatMap((result) => result.data?.projects ?? []),
  });
  const [open, setOpen] = useState(false);
  const list = useId();
  if (archived.length === 0) return null;
  return (
    <>
      <div className="grp">
        <button
          type="button"
          className="grp-fold"
          aria-expanded={open}
          aria-controls={list}
          onClick={() => setOpen((was) => !was)}
        >
          <ChevronIcon />
          Archived
          <span className="grp-n">{archived.length}</span>
        </button>
      </div>
      {open ? (
        <section id={list} aria-label="Archived projects" className="projects archived">
          {archived.map((project) => (
            <ArchivedRow key={projectKey(project)} project={project} />
          ))}
        </section>
      ) : null}
    </>
  );
}
