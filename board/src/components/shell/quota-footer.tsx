import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { useStore } from "../../lib/store";
import { connectionStore } from "../../lib/connection";
import { quotaQuery } from "../../lib/queries";
import { daemonTooltip, quotaFooterView, weeklyWindow } from "../../lib/quota";

// A status dot, a tiny meter, "27% week left" and "resets Tue 14:00". The
// daemon's live or stale state lives in the tooltip only. A click opens Health.
export function QuotaFooter() {
  const { data } = useSuspenseQuery(quotaQuery());
  const connection = useStore(connectionStore);
  const view = quotaFooterView(data);
  const tooltip = daemonTooltip(connection, weeklyWindow(data));

  if (!view) {
    return (
      <Link to="/health" className="quota" title={tooltip} data-testid="quota-footer">
        <span className="dot grey" />
        <span className="quota-text">
          <span className="quota-label">No quota data</span>
        </span>
      </Link>
    );
  }

  return (
    <Link to="/health" className="quota" title={tooltip} data-testid="quota-footer">
      <span className={`dot ${view.dot}`} data-dot={view.dot} />
      <span className="quota-text">
        <span className="quota-label">{view.label}</span>
        {view.resets ? <span className="quota-resets">{view.resets}</span> : null}
        <span className="meter" aria-hidden="true">
          <span
            className={`meter-fill ${view.dot}`}
            style={{ width: `${view.percentLeft ?? 0}%` }}
          />
        </span>
      </span>
    </Link>
  );
}
