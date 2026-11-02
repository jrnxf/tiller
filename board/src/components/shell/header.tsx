import { useSuspenseQuery } from "@tanstack/react-query";
import { Link, useRouterState } from "@tanstack/react-router";

import { connectionStore } from "../../lib/connection";
import { machinesQuery } from "../../lib/queries";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import type { Density, ThemePreference } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { CloseIcon, MenuIcon, SearchIcon } from "./icons";

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
  onOpenPalette: () => void;
}

export function Header({ activeHost, onOpenMenu, onOpenPalette }: HeaderProps) {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const { data: machines } = useSuspenseQuery(machinesQuery());
  const connection = useStore(connectionStore);
  const prefs = useStore(prefsStore);
  const host = machines.machines.find((machine) => machine.id === activeHost);

  return (
    <header className="header">
      <button
        type="button"
        className="icon-button menu"
        aria-label="Open the sidebar"
        onClick={onOpenMenu}
      >
        <MenuIcon />
      </button>
      <h1>{TITLES[pathname] ?? "Tiller"}</h1>
      {activeHost ? (
        <Link
          to="."
          search={(prev) => ({ ...prev, host: undefined })}
          className="chip"
          aria-label={`Remove the ${host?.label ?? "machine"} filter`}
          data-testid="host-chip"
        >
          {host?.label ?? "unknown host"}
          <CloseIcon />
        </Link>
      ) : null}
      <span className="sp" />
      <span
        className="pill"
        role="status"
        aria-live="polite"
        hidden={connection !== "reconnecting"}
        data-testid="degraded-pill"
      >
        Reconnecting - showing the last known state
      </span>
      <div className="seg theme" role="group" aria-label="Theme">
        {THEMES.map((theme) => (
          <button
            key={theme.value}
            type="button"
            className={prefs.theme === theme.value ? "on" : undefined}
            aria-pressed={prefs.theme === theme.value}
            onClick={() => updatePrefs({ theme: theme.value })}
          >
            {theme.label}
          </button>
        ))}
      </div>
      <div className="seg density" role="group" aria-label="Density">
        {DENSITIES.map((density) => (
          <button
            key={density.value}
            type="button"
            className={prefs.density === density.value ? "on" : undefined}
            aria-pressed={prefs.density === density.value}
            onClick={() => updatePrefs({ density: density.value })}
          >
            {density.label}
          </button>
        ))}
      </div>
      <button type="button" className="palette-button" onClick={onOpenPalette}>
        <SearchIcon />
        <span>Search</span>
        <kbd>⌘K</kbd>
      </button>
    </header>
  );
}
