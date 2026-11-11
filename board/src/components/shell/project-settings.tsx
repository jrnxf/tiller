import { useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { EditProject } from "./project-edit";
import { ProjectDialog } from "./project-dialog";

// One project's settings on this host: today the fields `tiller project edit`
// sets. Further sections of the same project (its grants) join this dialog as
// tabs above the body, each with its own save.
export function ProjectSettings({
  project,
  onClose,
  onSaved,
}: {
  project: Project;
  onClose: () => void;
  onSaved: (receiptKey: string, text: string) => void;
}) {
  const first = useRef<HTMLInputElement>(null);
  const [pending, setPending] = useState(false);
  return (
    <ProjectDialog
      name="project-settings"
      className="pa-dialog"
      pending={pending}
      onClose={onClose}
      initial={first}
    >
      <h2 id="project-settings-dialog-title">Edit {project.name}</h2>
      <p id="project-settings-dialog-body">
        Changes this project on this host, as <code>tiller project edit</code> does. The id{" "}
        <code>{project.id}</code> stays fixed.
      </p>
      <EditProject
        project={project}
        first={first}
        onPending={setPending}
        onClose={onClose}
        onSaved={(key) => onSaved(key, "Project saved")}
      />
    </ProjectDialog>
  );
}
