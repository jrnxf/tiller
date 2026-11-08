import { useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import type { CSSProperties } from "react";

import type { Contribution } from "../../lib/api-types";
import { projectColor } from "../../lib/project-color";
import { contributionsQuery, projectsQuery } from "../../lib/queries";
import { formatLisbonMoment } from "./format";
import { PrIcon } from "./icons";
import "./board.css";
import { useNamedProjects } from "./peer-state";

const STATE_TONE: Record<string, string> = { open: "ok", merged: "acc", closed: "" };

function Row({ pr, color, name }: { pr: Contribution; color: string; name: string }) {
  return (
    <div className="run" data-testid="contribution">
      <div className="rh">
        <PrIcon />
        <a href={pr.url} target="_blank" rel="noreferrer">
          {pr.repo}#{pr.number}
        </a>
        <span className={`lab sm ${STATE_TONE[pr.state] ?? ""}`}>{pr.state}</span>
        <span className="sp" />
        <span className="pdot" style={{ "--pc": color } as CSSProperties} />
        <span className="muted">{name}</span>
      </div>
      <p>
        <Link to="/cards/$cardId" params={{ cardId: pr.card_id }}>
          {pr.card_display_id}
        </Link>{" "}
        {pr.card_title}. Published {formatLisbonMoment(pr.linked_at)}
        {pr.merged_at ? `, merged ${formatLisbonMoment(pr.merged_at)}` : ""}.
      </p>
    </div>
  );
}

// Every PR published from a card, newest first, narrowed to one project from
// the URL so a project's contributions can be linked.
export function ContributionsView({ project }: { project: string | undefined }) {
  const { data } = useSuspenseQuery(contributionsQuery(project));
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const navigate = useNavigate();
  const local = projects.projects.filter((candidate) => !candidate.host);
  const select = (value: string | undefined) =>
    void navigate({ to: "/contributions", search: { project: value } });
  const nameOf = (id: string) => local.find((candidate) => candidate.id === id)?.name ?? id;
  const colorOf = (id: string) => projectColor(local.findIndex((candidate) => candidate.id === id));

  return (
    <div className="card-page" data-testid="contributions">
      <h4>Contributions</h4>
      <div className="seg" role="group" aria-label="Project" style={{ marginBottom: 18 }}>
        <button
          type="button"
          aria-pressed={project === undefined}
          onClick={() => select(undefined)}
        >
          All projects
        </button>
        {local.map((candidate) => (
          <button
            key={candidate.id}
            type="button"
            aria-pressed={project === candidate.id}
            onClick={() => select(candidate.id)}
          >
            {candidate.name}
          </button>
        ))}
      </div>
      <div className="sect">
        <div className="sect-h">
          Published PRs
          <span className="c">{data.contributions.length} newest first</span>
        </div>
        {data.contributions.length > 0 ? (
          data.contributions.map((pr) => (
            <Row key={pr.id} pr={pr} color={colorOf(pr.project_id)} name={nameOf(pr.project_id)} />
          ))
        ) : (
          <div className="run">
            <p style={{ margin: 0 }}>
              {project
                ? "This project has published no PRs yet."
                : "No PRs have been published yet."}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
