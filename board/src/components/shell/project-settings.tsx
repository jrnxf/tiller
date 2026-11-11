import { useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { Segmented } from "../ui/segmented";
import { ProjectDialog } from "./project-dialog";
import { EditProject } from "./project-edit";
import { GrantsTab } from "./project-grants";

type Tab = "grants" | "edit";

// One project's settings on this host, as tabs: its grants, as
// `tiller grant create` and `tiller grant revoke` manage them, and the fields
// `tiller project edit` sets. The dialog opens on the grants, so the whole
// policy is in view before one more grant joins it.
export function ProjectSettings({
  project,
  onClose,
  onSaved,
}: {
  project: Project;
  onClose: () => void;
  onSaved: (receiptKey: string, text: string) => void;
}) {
  const newGrant = useRef<HTMLButtonElement>(null);
  const firstField = useRef<HTMLInputElement>(null);
  const [pending, setPending] = useState(false);
  const [tab, setTab] = useState<Tab>("grants");
  // The edit form reads the project when it first opens, and keeps what is
  // typed in it while the grants tab is in front.
  const [editSeen, setEditSeen] = useState(false);
  return (
    <ProjectDialog
      name="project-settings"
      className="pa-dialog ps-dialog"
      pending={pending}
      onClose={onClose}
      initial={newGrant}
    >
      <h2 id="project-settings-dialog-title">{project.name}</h2>
      <Segmented
        className="ps-tabs"
        label="Project settings"
        options={[
          { value: "grants" as const, label: "Grants" },
          { value: "edit" as const, label: "Edit" },
        ]}
        value={tab}
        disabled={pending}
        onChange={(next) => {
          setTab(next);
          if (next === "edit") setEditSeen(true);
        }}
      />
      <div hidden={tab !== "grants"}>
        <GrantsTab project={project} first={newGrant} onClose={onClose} />
      </div>
      {editSeen ? (
        <div hidden={tab !== "edit"}>
          <p className="ps-edit-intro">
            Changes this project on this host, as <code>tiller project edit</code> does. The id{" "}
            <code>{project.id}</code> stays fixed.
          </p>
          <EditProject
            project={project}
            first={firstField}
            onPending={setPending}
            onClose={onClose}
            onSaved={(key) => onSaved(key, "Project saved")}
          />
        </div>
      ) : null}
    </ProjectDialog>
  );
}
