import type { ReactNode } from "react";
import { useCallback, useEffect, useState } from "react";

import { applyAppearance, prefsStore } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { CommandPalette } from "./command-palette";
import { Header } from "./header";
import { Sidebar } from "./sidebar";

// Keeps <html> in step with the theme and density preferences, and with the
// system theme while the preference is "system".
function useAppearance() {
  const prefs = useStore(prefsStore);
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => applyAppearance(prefs, media.matches, document.documentElement);
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [prefs]);
}

export function Shell({
  activeHost,
  children,
}: {
  activeHost: string | undefined;
  children: ReactNode;
}) {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  useAppearance();

  const closePalette = useCallback(() => setPaletteOpen(false), []);
  const closeMenu = useCallback(() => setMenuOpen(false), []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteOpen((open) => !open);
      } else if (event.key === "Escape") {
        setMenuOpen(false);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <div className={`shell${menuOpen ? " menu-open" : ""}`}>
      <Sidebar
        activeHost={activeHost}
        onNavigate={closeMenu}
        onOpenPalette={() => setPaletteOpen(true)}
      />
      <div className="nav-scrim" onClick={closeMenu} aria-hidden="true" />
      <div className="main">
        <Header activeHost={activeHost} onOpenMenu={() => setMenuOpen(true)} />
        <main className="content">{children}</main>
      </div>
      <CommandPalette open={paletteOpen} onClose={closePalette} activeHost={activeHost} />
    </div>
  );
}
