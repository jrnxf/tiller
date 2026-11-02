import { useSuspenseQuery } from "@tanstack/react-query";

import { projectsQuery } from "../../lib/queries";
import { toggleProjectHidden, updatePrefs, prefsStore } from "../../lib/prefs";
import type { ViewKnob } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { boardQuery } from "../../lib/queries";
import { EyeIcon, EyeOffIcon } from "./icons";

const VIEWS: { value: ViewKnob; label: string }[] = [
  { value: "all", label: "All projects" },
  { value: "focus", label: "Focus" },
];

export function ProjectsSection() {
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const prefs = useStore(prefsStore);

  const focused =
    prefs.view === "focus"
      ? (projects.projects.find((project) => project.id === prefs.focusProject) ??
        projects.projects[0])
      : undefined;

  return (
    <section className="sec" aria-labelledby="projects-heading">
      <h4 id="projects-heading">Projects</h4>
      <div className="seg" role="group" aria-label="View">
        {VIEWS.map((view) => (
          <button
            key={view.value}
            type="button"
            className={prefs.view === view.value ? "on" : undefined}
            aria-pressed={prefs.view === view.value}
            onClick={() => updatePrefs({ view: view.value })}
          >
            {view.label}
          </button>
        ))}
      </div>
      {projects.projects.map((project) => {
        const open = board.rows.filter(
          (row) => row.project_id === project.id && row.column !== "done",
        ).length;
        if (prefs.view === "focus") {
          const on = focused?.id === project.id;
          return (
            <button
              key={project.id}
              type="button"
              className={`row${on ? " on" : ""}`}
              aria-pressed={on}
              onClick={() => updatePrefs({ focusProject: project.id })}
            >
              <span className="row-name">{project.name}</span>
              <span className="n">{open || ""}</span>
            </button>
          );
        }
        const hidden = prefs.hiddenProjects.includes(project.id);
        return (
          <div key={project.id} className={`row project${hidden ? " hidden" : ""}`}>
            <span className="row-name">{project.name}</span>
            <span className="n">{open || ""}</span>
            <button
              type="button"
              className="icon-button"
              aria-label={`${hidden ? "Show" : "Hide"} ${project.name}`}
              aria-pressed={!hidden}
              onClick={() => toggleProjectHidden(project.id)}
            >
              {hidden ? <EyeOffIcon /> : <EyeIcon />}
            </button>
          </div>
        );
      })}
    </section>
  );
}
