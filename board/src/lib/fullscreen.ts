import { useSyncExternalStore } from "react";

// Whether this browser can put the page in full screen. iPhone Safari cannot
// (only a video), and there the phone's sheet is the full view.
export function canFullScreen(): boolean {
  try {
    return (
      document.fullscreenEnabled === true &&
      typeof document.documentElement.requestFullscreen === "function"
    );
  } catch {
    return false;
  }
}

function subscribeFullScreen(change: () => void) {
  document.addEventListener("fullscreenchange", change);
  return () => document.removeEventListener("fullscreenchange", change);
}

// Whether the whole page is in full screen. The stage goes full screen this
// way, and pins its card over the page, so a dialog, a menu or a toast that
// opens on the page's body still shows.
export function usePageFullScreen(): boolean {
  return useSyncExternalStore(
    subscribeFullScreen,
    () => document.fullscreenElement === document.documentElement,
    () => false,
  );
}

// The Keyboard Lock API, which only Chromium has. With Esc locked a tap of
// Esc reaches the page, so the pane gets it, and the browser leaves full
// screen only when Esc is held down.
type KeyboardLock = { lock(keys?: string[]): Promise<void>; unlock(): void };

function keyboardLock(): KeyboardLock | undefined {
  try {
    const keyboard = (navigator as Navigator & { keyboard?: Partial<KeyboardLock> }).keyboard;
    if (typeof keyboard?.lock !== "function" || typeof keyboard.unlock !== "function") {
      return undefined;
    }
    return keyboard as KeyboardLock;
  } catch {
    return undefined;
  }
}

let escHeld = false;
const escListeners = new Set<() => void>();

function setEscHeld(held: boolean) {
  if (escHeld === held) return;
  escHeld = held;
  for (const listener of escListeners) listener();
}

function releaseEsc() {
  if (!escHeld) return;
  setEscHeld(false);
  keyboardLock()?.unlock();
}

// Puts the page in full screen, and where the browser can lock Esc, locks it
// until the page leaves full screen. A lock the browser refuses keeps its own
// Esc, so the promise fails only when full screen does not open.
export async function enterPageFullScreen(): Promise<void> {
  await document.documentElement.requestFullscreen();
  const keyboard = keyboardLock();
  // Each way the lock fails goes to the console, so a tap of Esc that leaves
  // full screen has its reason in the developer tools.
  if (!keyboard) {
    console.debug("tiller: Esc is not held in full screen: this browser has no Keyboard Lock");
    return;
  }
  if (window.top !== window) {
    console.debug(
      "tiller: Esc may not hold in full screen: the board is in a frame, and the browser locks keys only for a top-level page",
    );
  }
  try {
    await keyboard.lock(["Escape"]);
  } catch (error) {
    console.debug(
      "tiller: Esc is not held in full screen: the browser refused the Keyboard Lock",
      error,
    );
    return;
  }
  if (document.fullscreenElement !== document.documentElement) {
    console.debug("tiller: Esc is not held: full screen ended before the Keyboard Lock took");
    keyboard.unlock();
    return;
  }
  setEscHeld(true);
  const left = () => {
    if (document.fullscreenElement === document.documentElement) return;
    document.removeEventListener("fullscreenchange", left);
    releaseEsc();
  };
  document.addEventListener("fullscreenchange", left);
}

function subscribeEscHeld(change: () => void) {
  escListeners.add(change);
  return () => {
    escListeners.delete(change);
  };
}

// Whether the page holds Esc in full screen, so a tap goes to the pane and
// leaving takes a held Esc.
export function useEscHeld(): boolean {
  return useSyncExternalStore(
    subscribeEscHeld,
    () => escHeld,
    () => false,
  );
}

// Leaves the page's full screen, if it is in it.
export function leavePageFullScreen() {
  releaseEsc();
  if (document.fullscreenElement === document.documentElement) {
    void document.exitFullscreen().catch(() => {});
  }
}
