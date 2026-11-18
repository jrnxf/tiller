import { useSuspenseQuery } from "@tanstack/react-query";
import type { HotkeyChord } from "@tanstack/react-hotkeys";
import { useHotkeys } from "@tanstack/react-hotkeys";
import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import type { ReactNode } from "react";

import { agentsQuery, attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { projectKey } from "../../lib/peers";
import { prefsStore } from "../../lib/prefs";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { attentionCount, dialogWaits, scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { useReplies } from "../attention/replies";
import { PrIcon } from "../board/icons";
import { ChatIcon } from "../chat/icons";
import { showToast } from "../board/toast";
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

export type ScreenPath =
  | "/"
  | "/agents"
  | "/chat"
  | "/contributions"
  | "/inbox"
  | "/memory"
  | "/health"
  | "/settings";

// Inbox comes first: what waits on the owner is the first thing to see.
// `key` is the second key of the screen's chord: G, then that key.
export const NAV: {
  to: ScreenPath;
  label: string;
  key: HotkeyChord;
  icon: ReactNode;
}[] = [
  { to: "/inbox", label: "Inbox", key: "I", icon: <InboxIcon /> },
  { to: "/", label: "Board", key: "B", icon: <BoardIcon /> },
  { to: "/chat", label: "Chat", key: "T", icon: <ChatIcon /> },
  { to: "/agents", label: "Agents", key: "A", icon: <AgentsIcon /> },
  { to: "/contributions", label: "Contributions", key: "C", icon: <PrIcon /> },
  { to: "/memory", label: "Memory", key: "M", icon: <MemoryIcon /> },
  { to: "/health", label: "Health", key: "H", icon: <PulseIcon /> },
  { to: "/settings", label: "Settings", key: "S", icon: <SettingsIcon /> },
];

// G, then a screen's key, opens that screen. The hotkey listens on the
// document, so its last key stops there and never reaches the window
// listeners of single-key shortcuts, such as I in the Agents view or A for
// Ask. A modal or a field the key is typed into keeps the key as its own.
function useNavHotkeys() {
  const navigate = useNavigate();
  useHotkeys(
    NAV.map((item) => ({
      hotkey: ["G", item.key],
      callback: (event: KeyboardEvent) => {
        if (shortcutsBlocked(event)) return;
        void navigate({ to: item.to, search: (prev) => ({ ...prev, owner: undefined }) });
      },
    })),
  );
}

// The projects the current view and machine filter show.
function useScoped(activeHost: string | undefined) {
  const { data: board } = useSuspenseQuery(boardQuery());
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const prefs = useStore(prefsStore);
  return { scoped: scopedProjects(prefs, projects.projects, board.rows, activeHost), projects };
}

// The ids of the projects the current view and machine filter show.
function useShownIds(activeHost: string | undefined): Set<string> {
  return new Set(useScoped(activeHost).scoped.map((project) => projectKey(project)));
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

// What waits on the owner, and the unread replies: the rows the Inbox counts.
function WaitingBadge({ activeHost }: { activeHost: string | undefined }) {
  const { data: attention } = useSuspenseQuery(attentionQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: agents } = useSuspenseQuery(agentsQuery());
  const shownIds = useShownIds(activeHost);
  const { replies } = useReplies(useScoped(activeHost).scoped);
  const waiting =
    attentionCount(attention, board.rows, shownIds, activeHost, dialogWaits(agents)) +
    replies.length;
  return waiting > 0 ? <span className="badge">{waiting}</span> : null;
}

// A reply that arrives while the owner is on another screen shows a toast
// with a link to it, so it is not missed. The replies already there when a
// project first comes into scope are not news.
function ReplyToasts({ activeHost }: { activeHost: string | undefined }) {
  const { scoped, projects } = useScoped(activeHost);
  const { replies, loading } = useReplies(scoped);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const seen = useRef(new Map<string, Set<string>>());
  useEffect(() => {
    if (loading) return;
    const fresh = [];
    const now = new Map<string, Set<string>>();
    for (const reply of replies) {
      const ids = now.get(reply.project) ?? new Set();
      ids.add(reply.id);
      now.set(reply.project, ids);
    }
    for (const project of scoped) {
      const before = seen.current.get(project.id);
      const ids = now.get(project.id) ?? new Set<string>();
      if (before)
        fresh.push(...replies.filter((r) => r.project === project.id && !before.has(r.id)));
      seen.current.set(project.id, new Set([...(before ?? []), ...ids]));
    }
    const newest = fresh[0];
    if (!newest || pathname === "/inbox") return;
    const name = projects.projects.find((project) => project.id === newest.project)?.name;
    showToast(
      "reply",
      fresh.length > 1
        ? `${fresh.length} new replies`
        : `${name ? `${name}: ` : ""}a reply to your ${newest.to === "ask" ? "ask" : "comment"}`,
      newest.question,
      null,
      { label: "Open in Inbox", item: newest.id },
    );
  });
  return null;
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
  useNavHotkeys();
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
            // Chat stays current on every project's chat under it.
            activeOptions={{ exact: item.to !== "/chat" }}
            // The Agents view and the chat page fold the sidebar to icons;
            // the name stays on hover, with the screen's chord.
            title={`${item.label} (G then ${item.key})`}
          >
            {item.icon}
            <span className="lbl">{item.label}</span>
            <NavCount to={item.to} label={item.label} activeHost={activeHost} />
          </Link>
        ))}
      </nav>
      <Section label="Replies" fallback={noCount}>
        <ReplyToasts activeHost={activeHost} />
      </Section>
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
