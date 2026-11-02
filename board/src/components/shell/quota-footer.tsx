import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import type { QuotaDot } from "../../lib/api-types";
import { useStore } from "../../lib/store";
import { connectionStore } from "../../lib/connection";
import { quotaQuery } from "../../lib/queries";
import { daemonTooltip, quotaFooterView, weeklyWindow } from "../../lib/quota";
import type { SectionFailure } from "../states/section";

const DOT_CLASS: Record<QuotaDot, string> = {
  green: "",
  amber: " warn",
  red: " bad",
  grey: " unk",
};

// A status dot, "27% week left", a tiny meter (the fill is what is used, the
// tick is how much of the week is gone) and "resets Tue 14:00". The daemon's
// live or stale state lives in the tooltip only. A click opens Health.
export function QuotaFooter() {
  const { data } = useSuspenseQuery(quotaQuery());
  const connection = useStore(connectionStore);
  const view = quotaFooterView(data);
  const tooltip = daemonTooltip(connection, weeklyWindow(data));

  if (!view) {
    return (
      <Link
        to="/health"
        className="qfoot"
        title={tooltip}
        aria-label={tooltip}
        data-testid="quota-footer"
      >
        <span className="qf-1">
          <i className="qdot unk" data-dot="grey" />
          <b>No quota data</b>
        </span>
      </Link>
    );
  }

  return (
    <Link
      to="/health"
      className="qfoot"
      title={tooltip}
      aria-label={`${tooltip} ${view.label}${view.resets ? `, ${view.resets}` : ""}.`}
      data-testid="quota-footer"
    >
      <span className="qf-1">
        <i className={`qdot${DOT_CLASS[view.dot]}`} data-dot={view.dot} />
        <b>{view.label}</b>
      </span>
      <span className="qf-2">
        <span className={`qmeter${view.percentUsed === null ? " unk" : ""}`} aria-hidden="true">
          {view.percentUsed === null ? null : <i style={{ width: `${view.percentUsed}%` }} />}
          {view.elapsedPercent === null ? null : <u style={{ left: `${view.elapsedPercent}%` }} />}
        </span>
        {view.resets ? <span>{view.resets}</span> : null}
      </span>
    </Link>
  );
}

// The footer when the quota read itself failed: the same box, grey, with the
// reason in the tooltip, so the sidebar keeps its shape.
export function QuotaUnavailable({ reason }: SectionFailure) {
  const tooltip = `Quota unavailable. ${reason}`;
  return (
    <Link
      to="/health"
      className="qfoot"
      title={tooltip}
      aria-label={tooltip}
      data-testid="quota-footer"
    >
      <span className="qf-1">
        <i className="qdot unk" data-dot="grey" />
        <b>Quota unavailable</b>
      </span>
    </Link>
  );
}
