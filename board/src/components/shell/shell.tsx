import type { ReactNode } from "react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { holdInert } from "../../lib/inert";
import { applyAppearance, prefsStore } from "../../lib/prefs";
import { blockShortcuts, dialogOpen, shortcutsBlocked } from "../../lib/shortcuts";
import { useStore } from "../../lib/store";
import { useDropEndedAnswers } from "../board/answer-strip";
import { GlobalNewCard, newCardStore, useNewCardTarget } from "../board/new-card";
import { projectKey } from "../../lib/peers";
import { ToastHost } from "../board/toast";
import { AskDialog } from "./ask-dialog";
import { askPrefillStore } from "./ask-prefill";
import { CommandPalette, NEW_CARD_QUERY } from "./command-palette";
import { Header } from "./header";
import { Sidebar } from "./sidebar";

// Keeps <html> in step with the theme and density preferences, and with the
// system theme while the preference is "system".
export function useAppearance() {
  const saved = useStore(prefsStore);
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => applyAppearance(saved, media.matches, document.documentElement);
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [saved]);
}

function isPlainKey(event: KeyboardEvent): boolean {
  return !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.repeat;
}

export function Shell({
  activeHost,
  children,
}: {
  activeHost: string | undefined;
  children: ReactNode;
}) {
  // null is closed; a string is the query the palette opens with.
  const [palette, setPalette] = useState<string | null>(null);
  const [askOpen, setAskOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const shellRef = useRef<HTMLDivElement>(null);
  const menuWasOpen = useRef(false);
  const mainRef = useRef<HTMLDivElement>(null);
  useAppearance();
  useDropEndedAnswers();

  const closePalette = useCallback(() => setPalette(null), []);
  // `C` files a card in the project in view. With every project in view it
  // opens the palette on "New card in", to pick one by a few letters.
  const target = useNewCardTarget();
  const targetKey = target ? projectKey(target) : null;
  const newCard = useCallback(() => {
    if (targetKey !== null) newCardStore.set(targetKey);
    else setPalette(NEW_CARD_QUERY);
  }, [targetKey]);
  // "Ask again" on a reply opens the box with its question in it.
  const prefill = useStore(askPrefillStore);
  const closeAsk = useCallback(() => {
    setAskOpen(false);
    askPrefillStore.set(null);
  }, []);
  const openAsk = useCallback(() => setAskOpen(true), []);
  const closeMenu = useCallback(() => setMenuOpen(false), []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        // It closes the palette too, but opens nothing over another dialog.
        event.preventDefault();
        const blocked = dialogOpen();
        setPalette((open) => (open !== null || blocked ? null : ""));
      } else if (event.key === "Escape") {
        // An Escape a dialog took closes that dialog only.
        if (!event.defaultPrevented && !dialogOpen()) setMenuOpen(false);
      } else if (event.key.toLowerCase() === "a" && isPlainKey(event) && !shortcutsBlocked(event)) {
        // The Ask button's shortcut, as long as nothing else is being typed into.
        event.preventDefault();
        setAskOpen(true);
      } else if (event.key.toLowerCase() === "c" && isPlainKey(event) && !shortcutsBlocked(event)) {
        event.preventDefault();
        newCard();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [newCard]);

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

  // The phone drawer is modal: the screen beside it is inert and no shortcut
  // acts on it. A dialog opened from the drawer holds the screen inert too, so
  // the screen comes back only when both let go, in either order.
  useLayoutEffect(() => {
    const main = mainRef.current;
    if (!menuOpen || !main) return;
    const release = holdInert([main]);
    const unblock = blockShortcuts();
    return () => {
      release();
      unblock();
    };
  }, [menuOpen]);

  // Focus moves into the drawer when it opens and back to its button when it
  // closes, so a keyboard user is never left behind a scrim.
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
        onOpenPalette={() => setPalette("")}
      />
      <div className="nav-scrim" onClick={closeMenu} aria-hidden="true" />
      <div ref={mainRef} className="main">
        <Header
          activeHost={activeHost}
          menuOpen={menuOpen}
          onOpenMenu={() => setMenuOpen(true)}
          onOpenAsk={() => setAskOpen(true)}
          newCardIn={target?.name ?? null}
          onNewCard={newCard}
        />
        <main className="content">{children}</main>
      </div>
      <AskDialog open={askOpen || prefill !== null} prefill={prefill} onClose={closeAsk} />
      <ToastHost />
      <GlobalNewCard />
      <CommandPalette
        open={palette !== null}
        initialQuery={palette ?? ""}
        onClose={closePalette}
        activeHost={activeHost}
        onAsk={openAsk}
        onNewCard={newCard}
      />
    </div>
  );
}
