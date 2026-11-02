import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { agentsQuery, attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { prefsStore } from "../../lib/prefs";
import { attentionCount, scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { AgentsIcon, BoardIcon, InboxIcon, MemoryIcon, PulseIcon, SearchIcon } from "./icons";
import { MachinesSection } from "./machines-section";
import { ProjectsSection } from "./projects-section";
import { QuotaFooter } from "./quota-footer";

const NAV: {
  to: "/" | "/agents" | "/inbox" | "/memory" | "/health";
  label: string;
  icon: ReactNode;
}[] = [
  { to: "/", label: "Board", icon: <BoardIcon /> },
  { to: "/agents", label: "Agents", icon: <AgentsIcon /> },
  { to: "/inbox", label: "Inbox", icon: <InboxIcon /> },
  { to: "/memory", label: "Memory", icon: <MemoryIcon /> },
  { to: "/health", label: "Health", icon: <PulseIcon /> },
];

export function Sidebar({
  activeHost,
  onNavigate,
  onOpenPalette,
}: {
  activeHost: string | undefined;
  onNavigate: () => void;
  onOpenPalette: () => void;
}) {
  const { data: attention } = useSuspenseQuery(attentionQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: agents } = useSuspenseQuery(agentsQuery());
  const prefs = useStore(prefsStore);
  const shownIds = new Set(
    scopedProjects(prefs, projects.projects, board.rows, activeHost).map((project) => project.id),
  );
  const waiting = attentionCount(attention, board.rows, shownIds, activeHost);
  const onHost = (hostId: string | null) => !activeHost || hostId === activeHost;
  const open = board.rows.filter(
    (row) => shownIds.has(row.project_id) && row.column !== "done" && onHost(row.host_id),
  ).length;
  const running = agents.orchestrators
    .flatMap((orchestrator) => [orchestrator, ...orchestrator.workers])
    .filter((agent) => onHost(agent.host_id)).length;

  return (
    <aside className="side" aria-label="Sidebar">
      <div className="ws">
        <div className="logo">T</div>
        <div className="name">tiller</div>
      </div>
      <button
        type="button"
        className="search"
        aria-label="Open command palette"
        onClick={() => {
          onNavigate();
          onOpenPalette();
        }}
      >
        <SearchIcon />
        <span>Search or run...</span>
        <kbd>⌘K</kbd>
      </button>
      <nav className="navs" aria-label="Screens" onClick={onNavigate}>
        {NAV.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            search={(prev) => prev}
            className="nav"
            activeProps={{ className: "nav", "aria-current": "page" }}
            activeOptions={{ exact: true }}
          >
            {item.icon}
            <span className="lbl">{item.label}</span>
            {item.to === "/" ? <span className="count">{open}</span> : null}
            {item.to === "/agents" ? <span className="count">{running}</span> : null}
            {item.to === "/inbox" && waiting > 0 ? (
              <span className="badge warn">{waiting}</span>
            ) : null}
          </Link>
        ))}
      </nav>
      <ProjectsSection activeHost={activeHost} />
      <MachinesSection activeHost={activeHost} />
      <div onClick={onNavigate} className="foot-q">
        <QuotaFooter />
      </div>
    </aside>
  );
}
