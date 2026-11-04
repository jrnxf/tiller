import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import type { QuotaDot } from "../../lib/api-types";
import { useStore } from "../../lib/store";
import { connectionStore } from "../../lib/connection";
import { quotaQuery } from "../../lib/queries";
import { daemonTooltip, quotaFooterView, weeklyWindow } from "../../lib/quota";
import { useTick } from "../board/ago";
import type { SectionFailure } from "../states/section";

const DOT_CLASS: Record<QuotaDot, string> = {
  green: "",
  amber: " warn",
  red: " bad",
  grey: " unk",
};

function Meter({
  left,
  marker,
  wide,
}: {
  left: number | null;
  marker: number | null;
  wide?: boolean;
}) {
  return (
    <span
      className={`qmeter${wide ? " wide" : ""}${left === null ? " unk" : ""}`}
      aria-hidden="true"
    >
      {left === null ? null : <i style={{ width: `${left}%` }} />}
      {marker === null ? null : <u style={{ left: `${marker}%` }} />}
    </span>
  );
}

// A status dot, "27% week left" and the runway ("empty in 3d 7h") when the
// pace runs the week out early, a meter (the fill is what is left, the tick is
// where an even spend would stand now), why the dot has its colour, the reset,
// and one row per model window. The daemon's live or stale state lives in the
// tooltip only. A click opens Health.
export function QuotaFooter() {
  const { data } = useSuspenseQuery(quotaQuery());
  const connection = useStore(connectionStore);
  useTick();
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
      aria-label={`${tooltip} ${view.label}. ${view.reason}${view.resets ? `, ${view.resets}` : ""}.`}
      data-testid="quota-footer"
    >
      <span className="qf-1">
        <i className={`qdot${DOT_CLASS[view.dot]}`} data-dot={view.dot} />
        <b>{view.label}</b>
        {view.runway ? <span className="qf-run">{view.runway}</span> : null}
      </span>
      <Meter left={view.percentLeft} marker={view.paceMarker} wide />
      <span className={`qf-why${DOT_CLASS[view.dot]}`}>
        <span>{view.reason}</span>
        {view.resets ? <span>{view.resets}</span> : null}
      </span>
      {view.rows.length > 0 ? (
        <span className="qrows">
          {view.rows.map((row) => (
            <span key={row.key} className="qrow" data-testid="quota-row">
              <span className="qr-name">{row.name}</span>
              <Meter left={row.percentLeft} marker={row.paceMarker} />
              <span className="qr-pct">
                {row.percentLeft === null ? "-" : `${Math.round(row.percentLeft)}%`}
              </span>
              <span className="qr-reset">{row.toReset ?? ""}</span>
            </span>
          ))}
        </span>
      ) : null}
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
