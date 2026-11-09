import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { agentsQuery } from "../../lib/queries";
import { RefreshIcon } from "../shell/icons";

// Shown when the daemon now runs a different tiller than the one that served
// this page, so the open tab still runs the old page code. It never reloads by
// itself: a reload would drop typing and open dialogs.
export function ReloadBanner() {
  const current = useQuery(agentsQuery()).data?.tiller_version;
  // The first version this page sees is the one that built it. A failed read
  // keeps the last known version, so a daemon restart on the same build stays quiet.
  const [loaded, setLoaded] = useState(current);
  if (loaded === undefined && current !== undefined) setLoaded(current);
  if (loaded === undefined || current === undefined || current === loaded) return null;
  return (
    <div className="banner warn" role="status" aria-live="polite" data-testid="reload-banner">
      <RefreshIcon />
      <div className="tx">
        <b>Tiller was updated to {current}.</b> This tab still runs {loaded}. Reload to get the new
        board.
      </div>
      <button type="button" className="btn" onClick={() => window.location.reload()}>
        Reload
      </button>
    </div>
  );
}
