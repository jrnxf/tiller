import { useEffect, useId, useRef, useState } from "react";

import {
  clearAppearancePreview,
  prefsStore,
  previewAppearance,
  updatePrefs,
} from "../../lib/prefs";
import type { Density, ThemePreference } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import { ChevronIcon, SettingsIcon } from "./icons";

interface Option<Value extends string> {
  value: Value;
  label: string;
}

interface Page {
  id: "theme" | "density";
  label: string;
  hint: string;
  options: Option<string>[];
}

const THEMES: Option<ThemePreference>[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const DENSITIES: Option<Density>[] = [
  { value: "comfortable", label: "Comfortable" },
  { value: "compact", label: "Compact" },
];

const PAGES: Page[] = [
  {
    id: "theme",
    label: "Theme",
    hint: "Follow the system, or stay light or dark",
    options: THEMES,
  },
  {
    id: "density",
    label: "Density",
    hint: "How much room rows and cards take",
    options: DENSITIES,
  },
];

function labelOf(page: Page, value: string): string {
  return page.options.find((option) => option.value === value)?.label ?? value;
}

// Mounted only while open, so each opening starts on the list of settings.
export function SettingsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  if (!open) return null;
  return <SettingsBody onClose={onClose} />;
}

function SettingsBody({ onClose }: { onClose: () => void }) {
  const prefs = useStore(prefsStore);
  // `null` is the list of settings; otherwise the sub-page of one of them.
  const [pageId, setPageId] = useState<Page["id"] | null>(null);
  const [index, setIndex] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);
  const opener = useRef<Element | null>(document.activeElement);
  const listId = useId();

  const page = PAGES.find((candidate) => candidate.id === pageId) ?? null;
  const rows = page ? page.options : PAGES;
  const selected = Math.min(index, rows.length - 1);
  const saved = (target: Page) => prefs[target.id];

  // Focus goes to the list on open and back to whatever opened Settings on
  // close, so a keyboard user keeps their place. Any preview ends with it.
  useEffect(() => {
    listRef.current?.focus();
    const back = opener.current;
    return () => {
      clearAppearancePreview();
      const active = document.activeElement;
      const lost = !active || active === document.body;
      if (lost && back instanceof HTMLElement && back.isConnected) back.focus();
    };
  }, []);

  // On a sub-page the option under the cursor or the keyboard is shown on the
  // whole board. Leaving the sub-page (or closing) clears it, which reverts.
  useEffect(() => {
    if (!page) return;
    const option = page.options[selected];
    if (option) previewAppearance({ [page.id]: option.value });
    return () => clearAppearancePreview();
  }, [page, selected]);

  const openPage = (target: Page) => {
    setPageId(target.id);
    setIndex(
      Math.max(
        0,
        target.options.findIndex((option) => option.value === saved(target)),
      ),
    );
    listRef.current?.focus();
  };

  const back = () => {
    const from = PAGES.findIndex((candidate) => candidate.id === pageId);
    setPageId(null);
    setIndex(Math.max(0, from));
    listRef.current?.focus();
  };

  const choose = (position: number) => {
    if (!page) {
      const target = PAGES[position];
      if (target) openPage(target);
      return;
    }
    const option = page.options[position];
    if (!option) return;
    clearAppearancePreview();
    updatePrefs({ [page.id]: option.value });
    back();
  };

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setIndex(Math.min(selected + 1, rows.length - 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setIndex(Math.max(selected - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      choose(selected);
    } else if (event.key === "Escape") {
      event.preventDefault();
      if (page) back();
      else onClose();
    } else if (event.key === "Tab") {
      // The list is the only stop; it is driven by the arrow keys.
      event.preventDefault();
    }
  };

  return (
    <div className="palette-backdrop settings-backdrop" onMouseDown={onClose}>
      <div
        className="palette settings"
        role="dialog"
        aria-modal="true"
        aria-label={page ? `Settings: ${page.label}` : "Settings"}
        onKeyDown={onKeyDown}
        // A click inside must not move focus off the list, or the keys go dead.
        onMouseDown={(event) => {
          event.stopPropagation();
          event.preventDefault();
        }}
      >
        <div className="settings-head">
          {page ? (
            <button
              type="button"
              className="ib sm"
              tabIndex={-1}
              aria-label="Back to settings"
              onClick={back}
            >
              <ChevronIcon />
            </button>
          ) : (
            <SettingsIcon />
          )}
          <b>{page ? page.label : "Settings"}</b>
          {page ? <span>{page.hint}</span> : null}
        </div>
        <ul
          ref={listRef}
          className="palette-list settings-list"
          id={listId}
          role="listbox"
          tabIndex={0}
          aria-label={page ? page.label : "Settings"}
          aria-activedescendant={`${listId}-${selected}`}
        >
          {page
            ? page.options.map((option, position) => (
                <li
                  key={option.value}
                  id={`${listId}-${position}`}
                  role="option"
                  aria-selected={position === selected}
                  data-current={saved(page) === option.value}
                  onMouseMove={() => setIndex(position)}
                  onClick={() => choose(position)}
                >
                  <span className="settings-check" aria-hidden="true">
                    {saved(page) === option.value ? "✓" : ""}
                  </span>
                  <span>{option.label}</span>
                </li>
              ))
            : PAGES.map((entry, position) => (
                <li
                  key={entry.id}
                  id={`${listId}-${position}`}
                  role="option"
                  aria-selected={position === selected}
                  onMouseMove={() => setIndex(position)}
                  onClick={() => choose(position)}
                >
                  <span>{entry.label}</span>
                  <span className="settings-value">{labelOf(entry, saved(entry))}</span>
                  <ChevronIcon />
                </li>
              ))}
        </ul>
      </div>
    </div>
  );
}
