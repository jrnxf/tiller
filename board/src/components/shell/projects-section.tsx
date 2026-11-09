import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Fragment, useState } from "react";
import type { CSSProperties } from "react";

import { isPeerRow, ownerKey, projectKey } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { toggleProjectHidden, updatePrefs, prefsStore, visibleProjects } from "../../lib/prefs";
import { projectsOnHost } from "../../lib/scope";
import type { ViewKnob } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { HostChip } from "../board/host-chip";
import { Segmented } from "../ui/segmented";
import type { SegmentedOption } from "../ui/segmented";
import { CheckIcon, ChevronIcon, EyeIcon, EyeOffIcon, FocusIcon, LayersIcon } from "./icons";
import { AddProject } from "./project-add";
import { ArchivedProjects } from "./projects-archived";
import { ProjectDetail, StageChip } from "./project-stage";
import { useNamedProjects } from "../board/peer-state";

const VIEWS: SegmentedOption<ViewKnob>[] = [
  { value: "all", label: "All projects", icon: <LayersIcon /> },
  { value: "focus", label: "Focus", icon: <FocusIcon /> },
];

// A weekly review turns overdue a day after it came due, with no event to
// say so, so a pre-launch project's row is read again now and then.
const OVERDUE_POLL_MS = 10 * 60_000;

export function ProjectsSection({ activeHost }: { activeHost: string | undefined }) {
  const projects = useNamedProjects(
    useSuspenseQuery({
      ...projectsQuery(),
      refetchInterval: (query) =>
        query.state.data?.projects.some((project) => project.stage === "pre-launch")
          ? OVERDUE_POLL_MS
          : false,
    }).data,
  );
  const { data: board } = useSuspenseQuery(boardQuery());
  // Attention only marks the projects that need you; the Inbox says when it
  // failed.
  const attention = useQuery(attentionQuery()).data;
  const prefs = useStore(prefsStore);
  // The projects whose stage panel is open.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());

  const shown = visibleProjects(prefs, projects.projects);
  const focusing = prefs.view === "focus";
  const focused = focusing ? shown[0] : undefined;
  const subset = !focusing && shown.length < projects.projects.length;

  const onHost = projectsOnHost(board.rows, activeHost);
  const projectOfCard = new Map(board.rows.map((row) => [row.id, ownerKey(row.project_id, row)]));
  const needsYou = new Set<string>();
  for (const item of attention?.items ?? []) {
    const project = projectOfCard.get(item.card_id);
    if (project) needsYou.add(project);
  }

  // Showing every project needs no note.
  const note = focusing
    ? focused
      ? `Focused on ${focused.name}. Other projects are hidden from every screen.`
      : "No projects yet."
    : subset
      ? `Showing ${shown.length} of ${projects.projects.length} projects. Hidden ones keep running.`
      : null;

  return (
    <>
      <div className="grp">
        <h4>Projects</h4>
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
        <AddProject />
      </div>
      <Segmented
        className="knob"
        label="View"
        options={VIEWS}
        value={prefs.view}
        onChange={(view) => updatePrefs({ view })}
      />
      <section aria-label="Projects" className="projects">
        {projects.projects.map((project, index) => {
          const key = projectKey(project);
          const open = board.rows.filter(
            (row) => ownerKey(row.project_id, row) === key && row.column !== "done",
          ).length;
          const hidden = !focusing && prefs.hiddenProjects.includes(key);
          const current = focusing && focused !== undefined && projectKey(focused) === key;
          const dim = (focusing && !current) || (onHost !== null && !onHost.has(key));
          const detailOpen = expanded.has(key);
          return (
            <Fragment key={key}>
              <div
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
                  onClick={() => updatePrefs({ view: "focus", focusProject: key })}
                >
                  <span className="pdot" />
                  <span className="nm">{project.name}</span>
                  <span className="pct">
                    {needsYou.has(key) ? <i /> : null}
                    {open}
                  </span>
                  <span className="pchips">
                    {isPeerRow(project) ? <HostChip entity={project} /> : null}
                    <StageChip project={project} />
                  </span>
                </button>
                <span className="pacts">
                  <button
                    type="button"
                    className="eye"
                    title={`${detailOpen ? "Hide" : "Show"} the stage of ${project.name}`}
                    aria-label={`Stage of ${project.name}`}
                    aria-expanded={detailOpen}
                    aria-controls={`stage-${key}`}
                    onClick={() =>
                      setExpanded((ids) => {
                        const next = new Set(ids);
                        if (!next.delete(key)) next.add(key);
                        return next;
                      })
                    }
                  >
                    <ChevronIcon />
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
                      onClick={() => toggleProjectHidden(key)}
                    >
                      {hidden ? <EyeOffIcon /> : <EyeIcon />}
                    </button>
                  )}
                </span>
              </div>
              {detailOpen ? <ProjectDetail project={project} id={`stage-${key}`} /> : null}
            </Fragment>
          );
        })}
      </section>
      {note ? <div className="knob-note">{note}</div> : null}
      <ArchivedProjects />
    </>
  );
}
