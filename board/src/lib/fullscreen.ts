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
