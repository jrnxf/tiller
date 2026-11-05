import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useRouterState } from "@tanstack/react-router";
import type { CSSProperties } from "react";

import { BoardFilterSegment, BoardFilterTabs } from "../board/filter";
import { projectKey } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { boardQuery, machinesQuery, projectsQuery } from "../../lib/queries";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { ReconnectingBanner } from "../states/reconnecting-banner";
import { Section } from "../states/section";
import { ChevronIcon, CloseIcon, LayersIcon, MenuIcon, ServerIcon } from "./icons";

const TITLES: Record<string, string> = {
  "/": "Board",
  "/agents": "Agents",
  "/contributions": "Contributions",
  "/inbox": "Inbox",
  "/memory": "Memory",
  "/health": "Health",
  "/settings": "Settings",
};

interface HeaderProps {
  activeHost: string | undefined;
  menuOpen: boolean;
  onOpenMenu: () => void;
}

const nothing = () => null;

// "All projects 4" or "2 of 4 projects", or the focused project's name.
function ScopeCrumb({ activeHost }: { activeHost: string | undefined }) {
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const prefs = useStore(prefsStore);
  const shown = scopedProjects(prefs, projects.projects, board.rows, activeHost);

  if (prefs.view === "focus" && shown[0]) {
    const index = projects.projects.findIndex(
      (project) => projectKey(project) === (shown[0] ? projectKey(shown[0]) : null),
    );
    return (
      <>
        <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
        <b>{shown[0].name}</b>
      </>
    );
  }
  const all = shown.length === projects.projects.length;
  return (
    <>
      <LayersIcon />
      <b>{all ? "All projects" : `${shown.length} of ${projects.projects.length} projects`}</b>
      <span className="cnt">{shown.length}</span>
    </>
  );
}

export function Header({ activeHost, menuOpen, onOpenMenu }: HeaderProps) {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  // The header only decorates with these, so a failed read leaves it working:
  // the machine chip still clears and the view can still be reset.
  const machines = useQuery(machinesQuery()).data?.machines ?? [];
  const projects = useQuery(projectsQuery()).data?.projects;
  const prefs = useStore(prefsStore);
  const host = machines.find((machine) => machine.id === activeHost);
  // Without the project list, any hidden project may still narrow the view.
  const narrowed =
    prefs.view === "focus" ||
    (projects
      ? projects.some((p) => prefs.hiddenProjects.includes(projectKey(p)))
      : prefs.hiddenProjects.length > 0);

  return (
    <>
      <header className="top">
        <button
          type="button"
          className="ib menu-btn"
          aria-label="Open the sidebar"
          aria-expanded={menuOpen}
          onClick={onOpenMenu}
        >
          <MenuIcon />
        </button>
        <div className="crumb">
          <Section label="Projects" fallback={nothing}>
            <ScopeCrumb activeHost={activeHost} />
          </Section>
          <ChevronIcon />
          <h1>{pathname.startsWith("/cards/") ? "Card" : (TITLES[pathname] ?? "Tiller")}</h1>
        </div>
        {activeHost ? (
          <Link
            to="."
            search={(prev) => ({ ...prev, host: undefined })}
            className="fchip"
            title="Remove the machine filter"
            aria-label={`Remove the ${host?.label ?? "machine"} filter`}
            data-testid="host-chip"
          >
            <ServerIcon />
            <span>{host?.label ?? "unknown host"}</span>
            <CloseIcon />
          </Link>
        ) : null}
        <div className="sp" />
        {narrowed ? (
          <button
            type="button"
            className="btn ghost"
            onClick={() => updatePrefs({ view: "all", hiddenProjects: [] })}
          >
            <LayersIcon />
            Show all projects
          </button>
        ) : null}
        {pathname === "/" ? (
          <Section label="Board filter" fallback={nothing}>
            <BoardFilterSegment />
          </Section>
        ) : null}
      </header>
      <ReconnectingBanner />
      {pathname === "/" ? (
        <Section label="Board filter" fallback={nothing}>
          <BoardFilterTabs />
        </Section>
      ) : null}
    </>
  );
}
