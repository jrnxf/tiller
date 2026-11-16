import { createStore } from "../../lib/store";

// Which lanes are folded, whether the Done column is folded to numbers, and
// whether a card shows the system events of its timeline.
// Per browser, like the theme: it is a way of looking, not shared state.
interface BoardLook {
  collapsed: string[];
  doneFolded: boolean;
  systemEvents: boolean;
}

const KEY = "tiller-board-look";

function load(): BoardLook {
  try {
    const parsed = JSON.parse(localStorage.getItem(KEY) ?? "null") as Partial<BoardLook> | null;
    return {
      collapsed: Array.isArray(parsed?.collapsed)
        ? parsed.collapsed.filter((id) => typeof id === "string")
        : [],
      doneFolded: parsed?.doneFolded === true,
      systemEvents: parsed?.systemEvents === true,
    };
  } catch {
    return { collapsed: [], doneFolded: false, systemEvents: false };
  }
}

export const boardLookStore = createStore<BoardLook>(load());

function save(look: BoardLook) {
  try {
    localStorage.setItem(KEY, JSON.stringify(look));
  } catch {
    // Blocked storage: the look lasts for this tab.
  }
}

export function toggleLane(projectId: string) {
  boardLookStore.update((look) => {
    const next = {
      ...look,
      collapsed: look.collapsed.includes(projectId)
        ? look.collapsed.filter((id) => id !== projectId)
        : [...look.collapsed, projectId],
    };
    save(next);
    return next;
  });
}

export function toggleDoneFolded() {
  boardLookStore.update((look) => {
    const next = { ...look, doneFolded: !look.doneFolded };
    save(next);
    return next;
  });
}

export function toggleSystemEvents() {
  boardLookStore.update((look) => {
    const next = { ...look, systemEvents: !look.systemEvents };
    save(next);
    return next;
  });
}

export function resetBoardLook() {
  const look = { collapsed: [], doneFolded: false, systemEvents: false };
  save(look);
  boardLookStore.set(look);
}
