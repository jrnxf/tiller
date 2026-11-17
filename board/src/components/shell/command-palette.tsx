import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { Fragment, useId, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { rankCommands } from "../../lib/commands";
import { ownerKey, ownerOf, projectKey, shownId, standingOf } from "../../lib/peers";
import { SendIcon } from "../attention/icons";
import { statusMark } from "../board/badge";
import { PrIcon, Status } from "../board/icons";
import { COLUMNS } from "../board/model";
import { newCardStore } from "../board/new-card";
import { roleName } from "../board/report";
import { ChatIcon } from "../chat/icons";
import type { Command } from "../../lib/commands";
import {
  agentsQuery,
  awayBriefQuery,
  boardQuery,
  machinesQuery,
  projectsQuery,
} from "../../lib/queries";
import { toggleProjectHidden, updatePrefs, prefsStore } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { Modal } from "../ui/modal";
import {
  AgentsIcon,
  BoardIcon,
  EyeIcon,
  EyeOffIcon,
  FocusIcon,
  InboxIcon,
  LayersIcon,
  MemoryIcon,
  PlusIcon,
  PulseIcon,
  ServerIcon,
  SettingsIcon,
} from "./icons";
import { useAmbiguousIds, useNamedProjects } from "../board/peer-state";
import { askPrefillStore } from "./ask-prefill";
import { MoonIcon, awayRequestStore } from "./away";
import { NAV } from "./sidebar";

interface PaletteProps {
  open: boolean;
  onClose: () => void;
  activeHost: string | undefined;
  // What the field holds when the palette opens, such as "New card in ".
  initialQuery?: string;
  // The header's Ask and New card, as their keys run them.
  onAsk: () => void;
  onNewCard: () => void;
}

type Jobs = Pick<PaletteProps, "onAsk" | "onNewCard">;

// The words the `C` key opens the palette with when no one project is in
// view: every "New card in <project>" command, to narrow by typing.
export const NEW_CARD_QUERY = "New card in ";

// A command that brings its own icon: a card shows its status mark.
interface PaletteCommand extends Command {
  icon?: ReactNode;
}

// The icon in front of a command, as in the approved mocks. Theme and density
// commands have none; their slot stays empty so the labels line up.
function iconFor(command: PaletteCommand) {
  if (command.icon) return command.icon;
  switch (command.id) {
    case "away":
      return <MoonIcon />;
    case "ask":
      return <SendIcon />;
    case "new-card":
      return <PlusIcon />;
    case "go-board":
      return <BoardIcon />;
    case "go-agents":
      return <AgentsIcon />;
    case "go-contributions":
      return <PrIcon />;
    case "go-inbox":
      return <InboxIcon />;
    case "go-memory":
      return <MemoryIcon />;
    case "go-health":
      return <PulseIcon />;
    case "go-settings":
      return <SettingsIcon />;
    case "view-all":
      return <LayersIcon />;
    case "view-focus":
      return <FocusIcon />;
  }
  if (command.id.startsWith("focus-")) return <FocusIcon />;
  if (command.id.startsWith("toggle-")) {
    return command.label.startsWith("Hide") ? <EyeOffIcon /> : <EyeIcon />;
  }
  if (command.id.startsWith("host-")) return <ServerIcon />;
  if (command.id.startsWith("ask-")) return <SendIcon />;
  if (command.id.startsWith("chat-")) return <ChatIcon />;
  if (command.id.startsWith("new-card-")) return <PlusIcon />;
  if (command.id.startsWith("agent-")) return <AgentsIcon />;
  return null;
}

// A screen's chord, as the sidebar binds it.
function chord(to: string): string[] | undefined {
  const item = NAV.find((entry) => entry.to === to);
  return item ? ["G", item.key] : undefined;
}

const COLUMN_NAME = new Map<string, string>(COLUMNS.map((column) => [column.id, column.name]));

function useCommands(
  activeHost: string | undefined,
  close: () => void,
  { onAsk, onNewCard }: Jobs,
): PaletteCommand[] {
  const navigate = useNavigate();
  // A read that failed only drops its own commands; going to a screen and
  // changing the look always work.
  const projects = useNamedProjects(useQuery(projectsQuery()).data);
  const machines = useQuery(machinesQuery()).data;
  const board = useQuery(boardQuery()).data;
  const agents = useQuery(agentsQuery()).data;
  const away = useQuery(awayBriefQuery()).data?.brief?.away === true;
  const ambiguous = useAmbiguousIds();
  const prefs = useStore(prefsStore);

  return useMemo(() => {
    const go =
      (to: "/" | "/agents" | "/contributions" | "/inbox" | "/memory" | "/health" | "/settings") =>
      () =>
        void navigate({ to, search: (prev) => prev });
    // The owner's jobs come first: they are what an empty palette offers.
    // One row per job; the row per project shows once a query names it.
    const commands: PaletteCommand[] = [
      { id: "ask", group: "Do", label: "Ask a project", keys: ["A"], run: onAsk },
      { id: "new-card", group: "Do", label: "New card", keys: ["C"], run: onNewCard },
      {
        id: "away",
        group: "Do",
        label: away ? "Away mode: show the limits, edit or end" : "Turn on away mode",
        keywords: "away leave absent",
        run: () => awayRequestStore.update((count) => count + 1),
      },
    ];
    const nameOf = new Map<string, string>();
    for (const project of projects?.projects ?? []) {
      const key = projectKey(project);
      // Two hosts may each have a project of one name; only then the host
      // tells them apart.
      const twin = projects?.projects.some((p) => p !== project && p.name === project.name);
      const name = twin && project.host ? `${project.name} (${project.host})` : project.name;
      nameOf.set(key, name);
      if (!standingOf(project, machines?.machines ?? []).canAct) continue;
      commands.push({
        id: `ask-${key}`,
        group: "Ask",
        label: `Ask ${name}`,
        keywords: "question orchestrator",
        searchOnly: true,
        run: () => askPrefillStore.set({ project: key, text: "" }),
      });
    }
    // A chat reads without acting, so a view-only peer's project has one too.
    for (const project of projects?.projects ?? []) {
      const key = projectKey(project);
      commands.push({
        id: `chat-${key}`,
        group: "Chat",
        label: `Chat with ${nameOf.get(key) ?? project.name}`,
        keywords: "orchestrator thread conversation messages",
        searchOnly: true,
        run: () =>
          void navigate({
            to: "/chat/$project",
            params: { project: project.id },
            search: (prev) => ({ ...prev, owner: ownerOf(project) }),
          }),
      });
    }
    for (const project of projects?.projects ?? []) {
      const key = projectKey(project);
      if (!standingOf(project, machines?.machines ?? []).canAct) continue;
      commands.push({
        id: `new-card-${key}`,
        group: "New card",
        label: `New card in ${nameOf.get(key) ?? project.name}`,
        keywords: "task file create add",
        searchOnly: true,
        run: () => newCardStore.set(key),
      });
    }
    commands.push(
      {
        id: "go-inbox",
        group: "Go to",
        label: "Inbox",
        keywords: "attention needs you",
        keys: chord("/inbox"),
        run: go("/inbox"),
      },
      { id: "go-board", group: "Go to", label: "Board", keys: chord("/"), run: go("/") },
      {
        id: "go-agents",
        group: "Go to",
        label: "Agents",
        keys: chord("/agents"),
        run: go("/agents"),
      },
      {
        id: "go-contributions",
        group: "Go to",
        label: "Contributions",
        keywords: "pull requests prs published",
        keys: chord("/contributions"),
        run: go("/contributions"),
      },
      {
        id: "go-memory",
        group: "Go to",
        label: "Memory",
        keys: chord("/memory"),
        run: go("/memory"),
      },
      {
        id: "go-health",
        group: "Go to",
        label: "Health",
        keywords: "quota",
        keys: chord("/health"),
        run: go("/health"),
      },
      {
        id: "go-settings",
        group: "Go to",
        label: "Settings",
        keywords: "theme density appearance",
        keys: chord("/settings"),
        run: go("/settings"),
      },
      {
        id: "theme-system",
        group: "Theme",
        label: "Theme: follow the system",
        run: () => updatePrefs({ theme: "system" }),
      },
      {
        id: "theme-light",
        group: "Theme",
        label: "Theme: light",
        run: () => updatePrefs({ theme: "light" }),
      },
      {
        id: "theme-dark",
        group: "Theme",
        label: "Theme: dark",
        run: () => updatePrefs({ theme: "dark" }),
      },
      {
        id: "density-comfortable",
        group: "Density",
        label: "Density: comfortable",
        run: () => updatePrefs({ density: "comfortable" }),
      },
      {
        id: "density-compact",
        group: "Density",
        label: "Density: compact",
        run: () => updatePrefs({ density: "compact" }),
      },
      {
        id: "view-all",
        group: "View",
        label: "Show all projects",
        run: () => updatePrefs({ view: "all" }),
      },
      {
        id: "view-focus",
        group: "View",
        label: "Focus on one project",
        run: () => updatePrefs({ view: "focus" }),
      },
    );
    for (const project of projects?.projects ?? []) {
      commands.push({
        id: `focus-${projectKey(project)}`,
        group: "View",
        label: `Focus ${project.name}${project.host && project.link ? ` (${project.host})` : ""}`,
        run: () => updatePrefs({ view: "focus", focusProject: projectKey(project) }),
      });
      commands.push({
        id: `toggle-${projectKey(project)}`,
        group: "View",
        label: `${prefs.hiddenProjects.includes(projectKey(project)) ? "Show" : "Hide"} ${project.name} in All projects`,
        run: () => toggleProjectHidden(projectKey(project)),
      });
    }
    for (const machine of machines?.machines ?? []) {
      commands.push({
        id: `host-${machine.id}`,
        group: "Machines",
        label: `Filter by ${machine.label}`,
        keywords: "machine host",
        run: () => void navigate({ to: ".", search: (prev) => ({ ...prev, host: machine.id }) }),
      });
    }
    if (activeHost) {
      commands.push({
        id: "host-clear",
        group: "Machines",
        label: "Clear the machine filter",
        run: () => void navigate({ to: ".", search: (prev) => ({ ...prev, host: undefined }) }),
      });
    }
    // Things to find. They show only for a query (lib/commands.ts).
    for (const row of board?.rows ?? []) {
      const owner = ownerOf(row);
      const project = nameOf.get(ownerKey(row.project_id, row)) ?? row.project_id;
      commands.push({
        id: `card-${owner ?? ""}/${row.id}`,
        group: "Cards",
        label: `${shownId(row, ambiguous)} ${row.title}`,
        keywords: project,
        hint: `${project} / ${row.needs_you ? "Needs you" : (COLUMN_NAME.get(row.column) ?? "")}`,
        icon: <Status mark={row.needs_you ? "needs_you" : statusMark(row)} />,
        searchOnly: true,
        run: () => void navigate({ to: "/", search: (prev) => ({ ...prev, card: row.id, owner }) }),
      });
    }
    for (const orchestrator of agents?.orchestrators ?? []) {
      for (const agent of [orchestrator, ...orchestrator.workers]) {
        const project = nameOf.get(ownerKey(agent.project_id ?? "", agent));
        const role = agent.role === "orchestrator" ? "Orchestrator" : roleName(agent.role);
        const card = agent.card_id
          ? board?.rows.find((row) => row.id === agent.card_id && ownerOf(row) === ownerOf(agent))
          : undefined;
        commands.push({
          id: `agent-${ownerOf(agent) ?? ""}/${agent.session_id}`,
          group: "Agents",
          label: card
            ? `${role} on ${shownId(card, ambiguous)} ${card.title}`
            : `${role} of ${project ?? "a project"}`,
          keywords: `${agent.name ?? ""} agent session terminal ${project ?? ""}`,
          hint: card ? project : undefined,
          searchOnly: true,
          run: () =>
            void navigate({
              to: "/agents",
              search: (prev) => ({ ...prev, agent: agent.session_id }),
            }),
        });
      }
    }
    return commands.map((command) => ({
      ...command,
      run: () => {
        close();
        command.run();
      },
    }));
  }, [
    navigate,
    projects,
    machines,
    board,
    agents,
    away,
    onAsk,
    onNewCard,
    ambiguous,
    prefs.hiddenProjects,
    activeHost,
    close,
  ]);
}

// Mounted only while open, so each opening starts with an empty query.
export function CommandPalette({ open, initialQuery = "", ...rest }: PaletteProps) {
  if (!open) return null;
  // A new opening query is a new opening: "New card" inside the palette
  // opens it again on its projects.
  return <PaletteBody key={initialQuery} initialQuery={initialQuery} {...rest} />;
}

function PaletteBody({
  onClose,
  activeHost,
  initialQuery = "",
  onAsk,
  onNewCard,
}: Omit<PaletteProps, "open">) {
  const commands = useCommands(activeHost, onClose, { onAsk, onNewCard });
  const [query, setQuery] = useState(initialQuery);
  const [index, setIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listId = useId();

  const results = useMemo(() => rankCommands(commands, query), [commands, query]);

  const selected = Math.min(index, Math.max(0, results.length - 1));

  return (
    // Focus goes to the input on open and back to whatever opened the palette
    // on close, so a keyboard user keeps their place. The input is the only
    // Tab stop: the arrow keys drive the list.
    <Modal
      backdrop="palette-backdrop"
      className="palette"
      label="Command palette"
      onClose={onClose}
      initialFocus={inputRef}
    >
      <input
        ref={inputRef}
        className="palette-input"
        role="combobox"
        aria-expanded="true"
        aria-controls={listId}
        aria-activedescendant={results[selected] ? `${listId}-${results[selected].id}` : undefined}
        placeholder="Search cards and agents, or type a command..."
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
          setIndex(0);
        }}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") {
            event.preventDefault();
            setIndex(Math.min(selected + 1, results.length - 1));
          } else if (event.key === "ArrowUp") {
            event.preventDefault();
            setIndex(Math.max(selected - 1, 0));
          } else if (event.key === "Enter") {
            event.preventDefault();
            results[selected]?.run();
          }
        }}
      />
      <ul className="palette-list" id={listId} role="listbox">
        {results.length === 0 ? (
          <li className="palette-empty">No card, agent or command matches</li>
        ) : null}
        {results.map((command, position) => (
          <Fragment key={command.id}>
            {position === 0 || results[position - 1]?.group !== command.group ? (
              <li className="palette-group" role="presentation">
                {command.group}
              </li>
            ) : null}
            <li
              id={`${listId}-${command.id}`}
              role="option"
              aria-selected={position === selected}
              onMouseMove={() => setIndex(position)}
              onClick={() => command.run()}
            >
              <span className="palette-icon">{iconFor(command)}</span>
              <span className="palette-label">{command.label}</span>
              {command.hint ? <span className="palette-hint">{command.hint}</span> : null}
              {command.keys ? (
                <span className="palette-keys" aria-hidden="true">
                  {command.keys.map((key) => (
                    <kbd key={key}>{key}</kbd>
                  ))}
                </span>
              ) : null}
            </li>
          </Fragment>
        ))}
      </ul>
    </Modal>
  );
}
