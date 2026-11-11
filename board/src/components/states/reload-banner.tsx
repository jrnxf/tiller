import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { agentsQuery } from "../../lib/queries";
import { createStore, useStore } from "../../lib/store";
import { RefreshIcon } from "../shell/icons";

// Whether this tab asked for a part of the board that the daemon no longer
// serves: an upgrade replaced the files a screen or the editor loads from.
const staleBoardStore = createStore(false);
const onError = () => staleBoardStore.set(true);

// Listens for a part of the board that failed to load. The screen that asked
// still shows its own error; the banner says what fixes it. Returns what stops
// the listening.
export function watchForStaleBoard(target: Window): () => void {
  target.addEventListener("vite:preloadError", onError);
  return () => target.removeEventListener("vite:preloadError", onError);
}

// Shown when the daemon now runs a different tiller than the one that served
// this page, so the open tab still runs the old page code. It never reloads by
// itself: a reload would drop typing and open dialogs.
export function ReloadBanner() {
  const current = useQuery(agentsQuery()).data?.tiller_version;
  const stale = useStore(staleBoardStore);
  // The first version this page sees is the one that built it. A failed read
  // keeps the last known version, so a daemon restart on the same build stays quiet.
  const [loaded, setLoaded] = useState(current);
  if (loaded === undefined && current !== undefined) setLoaded(current);
  const updated = loaded !== undefined && current !== undefined && current !== loaded;
  if (!updated && !stale) return null;
  return (
    <div className="banner warn" role="status" aria-live="polite" data-testid="reload-banner">
      <RefreshIcon />
      <div className="tx">
        {updated ? (
          <>
            <b>Tiller was updated to {current}.</b> This tab still runs {loaded}. Reload to get the
            new board.
          </>
        ) : (
          <>
            <b>Part of the board could not load.</b> Tiller may have been updated since this tab
            opened. Reload to get the current board.
          </>
        )}
      </div>
      <button type="button" className="btn" onClick={() => window.location.reload()}>
        Reload
      </button>
    </div>
  );
}
