import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { Fragment, useEffect, useId, useMemo, useRef, useState } from "react";

import { rankCommands } from "../../lib/commands";
import type { Command } from "../../lib/commands";
import { machinesQuery, projectsQuery } from "../../lib/queries";
import { toggleProjectHidden, updatePrefs, prefsStore } from "../../lib/prefs";
import { useStore } from "../../lib/store";
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
} from "./icons";

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
    case "go-inbox":
      return <InboxIcon />;
    case "go-memory":
      return <MemoryIcon />;
    case "go-health":
      return <PulseIcon />;
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
  const projects = useQuery(projectsQuery()).data;
  const machines = useQuery(machinesQuery()).data;
  const prefs = useStore(prefsStore);

  return useMemo(() => {
    const go = (to: "/" | "/agents" | "/inbox" | "/memory" | "/health") => () =>
      void navigate({ to, search: (prev) => prev });
    const commands: Command[] = [
      { id: "go-board", group: "Go to", label: "Board", run: go("/") },
      { id: "go-agents", group: "Go to", label: "Agents", run: go("/agents") },
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
        id: `focus-${project.id}`,
        group: "View",
        label: `Focus ${project.name}`,
        run: () => updatePrefs({ view: "focus", focusProject: project.id }),
      });
      commands.push({
        id: `toggle-${project.id}`,
        group: "View",
        label: `${prefs.hiddenProjects.includes(project.id) ? "Show" : "Hide"} ${project.name} in All projects`,
        run: () => toggleProjectHidden(project.id),
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
  const opener = useRef<Element | null>(document.activeElement);
  const listId = useId();

  const results = useMemo(() => rankCommands(commands, query), [commands, query]);

  // Focus goes to the input on open and back to whatever opened the palette
  // on close, so a keyboard user keeps their place.
  useEffect(() => {
    inputRef.current?.focus();
    const back = opener.current;
    return () => {
      // Only when focus fell to the page: a command may have moved it on purpose.
      const active = document.activeElement;
      const lost = !active || active === document.body;
      if (lost && back instanceof HTMLElement && back.isConnected) back.focus();
    };
  }, []);

  const selected = Math.min(index, Math.max(0, results.length - 1));

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="palette-input"
          role="combobox"
          aria-expanded="true"
          aria-controls={listId}
          aria-activedescendant={
            results[selected] ? `${listId}-${results[selected].id}` : undefined
          }
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
            } else if (event.key === "Escape") {
              event.preventDefault();
              onClose();
            } else if (event.key === "Tab") {
              // The input is the only stop; the list is driven by the arrow keys.
              event.preventDefault();
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
      </div>
    </div>
  );
}
