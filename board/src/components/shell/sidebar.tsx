import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { agentsQuery, attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { projectKey } from "../../lib/peers";
import { prefsStore } from "../../lib/prefs";
import { attentionCount, dialogWaits, scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { PrIcon } from "../board/icons";
import { countsByFilter, tilesInScope } from "../board/model";
import { Section } from "../states/section";
import {
  AgentsIcon,
  BoardIcon,
  InboxIcon,
  MemoryIcon,
  PulseIcon,
  SearchIcon,
  SettingsIcon,
} from "./icons";
import { Logo } from "./logo";
import { MachinesSection } from "./machines-section";
import { ProjectsSection } from "./projects-section";
import { QuotaFooter, QuotaUnavailable } from "./quota-footer";
import { useNamedProjects } from "../board/peer-state";

const NAV: {
  to: "/" | "/agents" | "/contributions" | "/inbox" | "/memory" | "/health" | "/settings";
  label: string;
  icon: ReactNode;
}[] = [
  { to: "/", label: "Board", icon: <BoardIcon /> },
  { to: "/agents", label: "Agents", icon: <AgentsIcon /> },
  { to: "/contributions", label: "Contributions", icon: <PrIcon /> },
  { to: "/inbox", label: "Inbox", icon: <InboxIcon /> },
  { to: "/memory", label: "Memory", icon: <MemoryIcon /> },
  { to: "/health", label: "Health", icon: <PulseIcon /> },
  { to: "/settings", label: "Settings", icon: <SettingsIcon /> },
];

// The ids of the projects the current view and machine filter show.
function useShownIds(activeHost: string | undefined): Set<string> {
  const { data: board } = useSuspenseQuery(boardQuery());
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const prefs = useStore(prefsStore);
  return new Set(
    scopedProjects(prefs, projects.projects, board.rows, activeHost).map((project) =>
      projectKey(project),
    ),
  );
}

// The counts beside the screen names read their own queries: one that cannot
// be read is left out, and the screen it belongs to says why.
function OpenCount({ activeHost }: { activeHost: string | undefined }) {
  const { data: board } = useSuspenseQuery(boardQuery());
  const shownIds = useShownIds(activeHost);
  // The header's Active tab, from the same list.
  const { active } = countsByFilter(tilesInScope(board, shownIds, activeHost));
  return <span className="count">{active}</span>;
}

function RunningCount({ activeHost }: { activeHost: string | undefined }) {
  const { data: agents } = useSuspenseQuery(agentsQuery());
  const running = agents.orchestrators
    .flatMap((orchestrator) => [orchestrator, ...orchestrator.workers])
    .filter((agent) => !activeHost || agent.host_id === activeHost).length;
  return <span className="count">{running}</span>;
}

function WaitingBadge({ activeHost }: { activeHost: string | undefined }) {
  const { data: attention } = useSuspenseQuery(attentionQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: agents } = useSuspenseQuery(agentsQuery());
  const shownIds = useShownIds(activeHost);
  const waiting = attentionCount(attention, board.rows, shownIds, activeHost, dialogWaits(agents));
  return waiting > 0 ? <span className="badge warn">{waiting}</span> : null;
}

const COUNTS: Partial<Record<string, (props: { activeHost: string | undefined }) => ReactNode>> = {
  "/": OpenCount,
  "/agents": RunningCount,
  "/inbox": WaitingBadge,
};

const noCount = () => null;

function NavCount({
  to,
  label,
  activeHost,
}: {
  to: string;
  label: string;
  activeHost: string | undefined;
}) {
  const Count = COUNTS[to];
  if (!Count) return null;
  return (
    <Section label={label} fallback={noCount}>
      <Count activeHost={activeHost} />
    </Section>
  );
}

export function Sidebar({
  activeHost,
  onNavigate,
  onOpenPalette,
}: {
  activeHost: string | undefined;
  onNavigate: () => void;
  onOpenPalette: () => void;
}) {
  return (
    <aside className="side" aria-label="Sidebar">
      <div className="ws">
        <Logo />
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
            // `owner` belongs to the card page; no other screen takes it.
            search={(prev) => ({ ...prev, owner: undefined })}
            className="nav"
            activeProps={{ className: "nav", "aria-current": "page" }}
            activeOptions={{ exact: true }}
            // The Agents view folds the sidebar to icons; the name stays on hover.
            title={item.label}
          >
            {item.icon}
            <span className="lbl">{item.label}</span>
            <NavCount to={item.to} label={item.label} activeHost={activeHost} />
          </Link>
        ))}
      </nav>
      <Section label="Projects">
        <ProjectsSection activeHost={activeHost} />
      </Section>
      <Section label="Machines">
        <MachinesSection activeHost={activeHost} />
      </Section>
      <div onClick={onNavigate} className="foot-q">
        <Section label="Quota" fallback={QuotaUnavailable}>
          <QuotaFooter />
        </Section>
      </div>
    </aside>
  );
}
