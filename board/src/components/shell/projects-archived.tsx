import { useQuery } from "@tanstack/react-query";
import { useCallback, useId, useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { retiredProjectsQuery } from "../../lib/queries";
import { formatWhen } from "../attention/format";
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
      </span>
      <span className="pacts">
        <button
          ref={opener}
          type="button"
          className="eye delete"
          title={`Delete ${project.name}`}
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

// The projects archived on this host, folded under the live ones. Archiving
// keeps a project's history; its delete is the one thing left to do with it.
export function ArchivedProjects() {
  const archived = useQuery(retiredProjectsQuery()).data?.projects ?? [];
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
            <ArchivedRow key={project.id} project={project} />
          ))}
        </section>
      ) : null}
    </>
  );
}
