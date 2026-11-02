import type { ReactNode } from "react";
import { useCallback, useEffect, useRef, useState } from "react";

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
  const shellRef = useRef<HTMLDivElement>(null);
  const menuWasOpen = useRef(false);
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

  // The drawer only exists on a phone-width layout; widening the window while
  // it is open must not leave the screen behind it inert.
  useEffect(() => {
    const media = window.matchMedia("(max-width: 820px)");
    const onChange = (event: MediaQueryListEvent) => {
      if (!event.matches) setMenuOpen(false);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);

  // The phone drawer is modal: focus moves into it when it opens and back to
  // its button when it closes, so a keyboard user is never left behind a scrim.
  useEffect(() => {
    const shell = shellRef.current;
    if (!shell) return;
    if (menuOpen) {
      shell.querySelector<HTMLElement>(".side .search")?.focus();
    } else if (menuWasOpen.current) {
      const active = document.activeElement;
      if (!active || active === document.body || shell.querySelector(".side")?.contains(active)) {
        shell.querySelector<HTMLElement>(".menu-btn")?.focus();
      }
    }
    menuWasOpen.current = menuOpen;
  }, [menuOpen]);

  return (
    <div ref={shellRef} className={`shell${menuOpen ? " menu-open" : ""}`}>
      <Sidebar
        activeHost={activeHost}
        onNavigate={closeMenu}
        onOpenPalette={() => setPaletteOpen(true)}
      />
      <div className="nav-scrim" onClick={closeMenu} aria-hidden="true" />
      <div className="main" inert={menuOpen}>
        <Header activeHost={activeHost} menuOpen={menuOpen} onOpenMenu={() => setMenuOpen(true)} />
        <main className="content">{children}</main>
      </div>
      <CommandPalette open={paletteOpen} onClose={closePalette} activeHost={activeHost} />
    </div>
  );
}
