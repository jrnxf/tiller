import { useSyncExternalStore } from "react";

// Whether this browser can put an element in full screen. iPhone Safari
// cannot (only a video), and there the phone's sheet is the full view.
export function canFullScreen(): boolean {
  try {
    return (
      document.fullscreenEnabled === true &&
      typeof Element.prototype.requestFullscreen === "function"
    );
  } catch {
    return false;
  }
}

function subscribeFullScreen(change: () => void) {
  document.addEventListener("fullscreenchange", change);
  return () => document.removeEventListener("fullscreenchange", change);
}

// The element in full screen, or null.
export function useFullScreenElement(): Element | null {
  return useSyncExternalStore(
    subscribeFullScreen,
    () => document.fullscreenElement ?? null,
    () => null,
  );
}

// Where a dialog or a toast goes: only the element in full screen and what it
// holds show, so there while one is, and the page's body otherwise.
export function useLayerRoot(): Element {
  return useFullScreenElement() ?? document.body;
}
