import { useSuspenseQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useId, useMemo, useRef, useState } from "react";

import { rankCommands } from "../../lib/commands";
import type { Command } from "../../lib/commands";
import { machinesQuery, projectsQuery } from "../../lib/queries";
import { toggleProjectHidden, updatePrefs, prefsStore } from "../../lib/prefs";
import { useStore } from "../../lib/store";

interface PaletteProps {
  open: boolean;
  onClose: () => void;
  activeHost: string | undefined;
}

function useCommands(activeHost: string | undefined, close: () => void): Command[] {
  const navigate = useNavigate();
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: machines } = useSuspenseQuery(machinesQuery());
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
        label: "View: all projects",
        run: () => updatePrefs({ view: "all" }),
      },
      {
        id: "view-focus",
        group: "View",
        label: "View: focus",
        run: () => updatePrefs({ view: "focus" }),
      },
    ];
    for (const project of projects.projects) {
      commands.push({
        id: `focus-${project.id}`,
        group: "Projects",
        label: `Focus on ${project.name}`,
        run: () => updatePrefs({ view: "focus", focusProject: project.id }),
      });
      commands.push({
        id: `toggle-${project.id}`,
        group: "Projects",
        label: `${prefs.hiddenProjects.includes(project.id) ? "Show" : "Hide"} ${project.name}`,
        run: () => toggleProjectHidden(project.id),
      });
    }
    for (const machine of machines.machines) {
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

  useEffect(() => {
    inputRef.current?.focus();
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
          placeholder="Type a command"
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
            }
          }}
        />
        <ul className="palette-list" id={listId} role="listbox">
          {results.length === 0 ? <li className="palette-empty">No command matches</li> : null}
          {results.map((command, position) => (
            <li
              key={command.id}
              id={`${listId}-${command.id}`}
              role="option"
              aria-selected={position === selected}
              className={position === selected ? "on" : undefined}
              onMouseMove={() => setIndex(position)}
              onClick={() => command.run()}
            >
              <span>{command.label}</span>
              <span className="palette-group">{command.group}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
