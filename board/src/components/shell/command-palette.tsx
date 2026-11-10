import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { Fragment, useId, useMemo, useRef, useState } from "react";

import { rankCommands } from "../../lib/commands";
import { projectKey } from "../../lib/peers";
import { PrIcon } from "../board/icons";
import type { Command } from "../../lib/commands";
import { machinesQuery, projectsQuery } from "../../lib/queries";
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
  PulseIcon,
  ServerIcon,
  SettingsIcon,
} from "./icons";
import { useNamedProjects } from "../board/peer-state";

interface PaletteProps {
  open: boolean;
  onClose: () => void;
  activeHost: string | undefined;
}

// The icon in front of a command, as in the approved mocks. Theme and density
// commands have none; their slot stays empty so the labels line up.
function iconFor(command: Command) {
  switch (command.id) {
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
  return null;
}

function useCommands(activeHost: string | undefined, close: () => void): Command[] {
  const navigate = useNavigate();
  // A read that failed only drops its own commands; going to a screen and
  // changing the look always work.
  const projects = useNamedProjects(useQuery(projectsQuery()).data);
  const machines = useQuery(machinesQuery()).data;
  const prefs = useStore(prefsStore);

  return useMemo(() => {
    const go =
      (to: "/" | "/agents" | "/contributions" | "/inbox" | "/memory" | "/health" | "/settings") =>
      () =>
        void navigate({ to, search: (prev) => prev });
    const commands: Command[] = [
      { id: "go-board", group: "Go to", label: "Board", run: go("/") },
      { id: "go-agents", group: "Go to", label: "Agents", run: go("/agents") },
      {
        id: "go-contributions",
        group: "Go to",
        label: "Contributions",
        keywords: "pull requests prs published",
        run: go("/contributions"),
      },
      {
        id: "go-inbox",
        group: "Go to",
        label: "Inbox",
        keywords: "attention needs you",
        run: go("/inbox"),
      },
      { id: "go-memory", group: "Go to", label: "Memory", run: go("/memory") },
      { id: "go-health", group: "Go to", label: "Health", keywords: "quota", run: go("/health") },
      {
        id: "go-settings",
        group: "Go to",
        label: "Settings",
        keywords: "theme density appearance",
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
    ];
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
    return commands.map((command) => ({
      ...command,
      run: () => {
        close();
        command.run();
      },
    }));
  }, [navigate, projects, machines, prefs.hiddenProjects, activeHost, close]);
}

// Mounted only while open, so each opening starts with an empty query.
export function CommandPalette({ open, onClose, activeHost }: PaletteProps) {
  if (!open) return null;
  return <PaletteBody onClose={onClose} activeHost={activeHost} />;
}

function PaletteBody({ onClose, activeHost }: Omit<PaletteProps, "open">) {
  const commands = useCommands(activeHost, onClose);
  const [query, setQuery] = useState("");
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
        placeholder="Type a command, or search..."
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
        {results.length === 0 ? <li className="palette-empty">No command matches</li> : null}
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
              <span>{command.label}</span>
            </li>
          </Fragment>
        ))}
      </ul>
    </Modal>
  );
}
