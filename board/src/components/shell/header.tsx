import { useSuspenseQuery } from "@tanstack/react-query";
import { Link, useRouterState } from "@tanstack/react-router";
import type { CSSProperties } from "react";

import { connectionStore } from "../../lib/connection";
import { projectColor } from "../../lib/project-color";
import { boardQuery, machinesQuery, projectsQuery } from "../../lib/queries";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import type { Density, ThemePreference } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { ChevronIcon, CloseIcon, LayersIcon, MenuIcon, RefreshIcon, ServerIcon } from "./icons";

const TITLES: Record<string, string> = {
  "/": "Board",
  "/agents": "Agents",
  "/inbox": "Inbox",
  "/memory": "Memory",
  "/health": "Health",
};

const THEMES: { value: ThemePreference; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const DENSITIES: { value: Density; label: string }[] = [
  { value: "comfortable", label: "Comfortable" },
  { value: "compact", label: "Compact" },
];

interface HeaderProps {
  activeHost: string | undefined;
  onOpenMenu: () => void;
}

// "All projects 4" or "2 of 4 projects", or the focused project's name.
function ScopeCrumb({ activeHost }: { activeHost: string | undefined }) {
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const prefs = useStore(prefsStore);
  const shown = scopedProjects(prefs, projects.projects, board.rows, activeHost);

  if (prefs.view === "focus" && shown[0]) {
    const index = projects.projects.findIndex((project) => project.id === shown[0]?.id);
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

export function Header({ activeHost, onOpenMenu }: HeaderProps) {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const { data: machines } = useSuspenseQuery(machinesQuery());
  const connection = useStore(connectionStore);
  const prefs = useStore(prefsStore);
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const host = machines.machines.find((machine) => machine.id === activeHost);
  const narrowed =
    prefs.view === "focus" || projects.projects.some((p) => prefs.hiddenProjects.includes(p.id));

  return (
    <>
      <header className="top">
        <button
          type="button"
          className="ib menu-btn"
          aria-label="Open the sidebar"
          onClick={onOpenMenu}
        >
          <MenuIcon />
        </button>
        <div className="crumb">
          <ScopeCrumb activeHost={activeHost} />
          <ChevronIcon />
          <h1>{TITLES[pathname] ?? "Tiller"}</h1>
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
        <div className="seg hide-sm" role="group" aria-label="Theme">
          {THEMES.map((theme) => (
            <button
              key={theme.value}
              type="button"
              aria-pressed={prefs.theme === theme.value}
              onClick={() => updatePrefs({ theme: theme.value })}
            >
              {theme.label}
            </button>
          ))}
        </div>
        <div className="seg hide-sm" role="group" aria-label="Density">
          {DENSITIES.map((density) => (
            <button
              key={density.value}
              type="button"
              aria-pressed={prefs.density === density.value}
              onClick={() => updatePrefs({ density: density.value })}
            >
              {density.label}
            </button>
          ))}
        </div>
      </header>
      <div
        className="banner warn"
        role="status"
        aria-live="polite"
        hidden={connection !== "reconnecting"}
        data-testid="degraded-pill"
      >
        <RefreshIcon />
        <div className="tx">
          <b>Daemon reconnecting.</b> Showing the last known state; answers are queued until it is
          back.
        </div>
      </div>
    </>
  );
}
