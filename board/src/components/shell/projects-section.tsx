import { useSuspenseQuery } from "@tanstack/react-query";
import type { CSSProperties } from "react";

import { projectColor } from "../../lib/project-color";
import { attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { toggleProjectHidden, updatePrefs, prefsStore, visibleProjects } from "../../lib/prefs";
import { projectsOnHost } from "../../lib/scope";
import type { ViewKnob } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { CheckIcon, EyeIcon, EyeOffIcon, FocusIcon, LayersIcon } from "./icons";

const VIEWS: { value: ViewKnob; label: string }[] = [
  { value: "all", label: "All projects" },
  { value: "focus", label: "Focus" },
];

export function ProjectsSection({ activeHost }: { activeHost: string | undefined }) {
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: attention } = useSuspenseQuery(attentionQuery());
  const prefs = useStore(prefsStore);

  const shown = visibleProjects(prefs, projects.projects);
  const focusing = prefs.view === "focus";
  const focused = focusing ? shown[0] : undefined;
  const subset = !focusing && shown.length < projects.projects.length;

  const onHost = projectsOnHost(board.rows, activeHost);
  const projectOfCard = new Map(board.rows.map((row) => [row.id, row.project_id]));
  const needsYou = new Set<string>();
  for (const item of attention.items) {
    const project = projectOfCard.get(item.card_id);
    if (project) needsYou.add(project);
  }

  const note = focusing
    ? focused
      ? `Focused on ${focused.name}. Other projects are hidden from every screen.`
      : "No projects yet."
    : subset
      ? `Showing ${shown.length} of ${projects.projects.length} projects. Hidden ones keep running.`
      : `Showing all ${projects.projects.length} projects at once.`;

  return (
    <>
      <div className="grp">
        <h4>View</h4>
        <span className="sp" />
        {subset || focusing ? (
          <button
            type="button"
            className="lnk"
            onClick={() => updatePrefs({ view: "all", hiddenProjects: [] })}
          >
            Show all
          </button>
        ) : null}
      </div>
      <div className="knob" role="group" aria-label="View">
        {VIEWS.map((view) => (
          <button
            key={view.value}
            type="button"
            aria-pressed={prefs.view === view.value}
            onClick={() => updatePrefs({ view: view.value })}
          >
            {view.value === "all" ? <LayersIcon /> : <FocusIcon />}
            {view.label}
          </button>
        ))}
      </div>
      <section aria-label="Projects" className="projects">
        {projects.projects.map((project, index) => {
          const open = board.rows.filter(
            (row) => row.project_id === project.id && row.column !== "done",
          ).length;
          const hidden = !focusing && prefs.hiddenProjects.includes(project.id);
          const current = focusing && focused?.id === project.id;
          const dim = (focusing && !current) || (onHost !== null && !onHost.has(project.id));
          return (
            <div
              key={project.id}
              className="prow"
              style={{ "--pc": projectColor(index) } as CSSProperties}
              data-off={hidden}
              data-dim={dim}
              aria-current={current ? "true" : undefined}
            >
              <button
                type="button"
                className="pbtn"
                title={`Focus ${project.name}`}
                aria-pressed={focusing ? current : undefined}
                onClick={() => updatePrefs({ view: "focus", focusProject: project.id })}
              >
                <span className="pdot" />
                <span className="nm">{project.name}</span>
                <span className="pct">
                  {needsYou.has(project.id) ? <i /> : null}
                  {open}
                </span>
              </button>
              {focusing ? (
                <span className={current ? "eye on" : "eye sp"}>
                  {current ? <CheckIcon /> : null}
                </span>
              ) : (
                <button
                  type="button"
                  className="eye"
                  title={`${hidden ? "Show" : "Hide"} ${project.name} in All projects`}
                  aria-label={`${hidden ? "Show" : "Hide"} ${project.name}`}
                  aria-pressed={!hidden}
                  onClick={() => toggleProjectHidden(project.id)}
                >
                  {hidden ? <EyeOffIcon /> : <EyeIcon />}
                </button>
              )}
            </div>
          );
        })}
      </section>
      <div className="knob-note">{note}</div>
    </>
  );
}
