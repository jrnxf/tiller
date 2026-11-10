import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useCallback } from "react";
import type { ReactNode } from "react";

import type { RecapResponse } from "../../lib/api-types";
import { formatLisbon } from "../../lib/clock";
import { queryKeyFor, recapPath, recapQuery } from "../../lib/queries";
import { ActionFailure } from "./action-failure";
import { sendMutation } from "./api";
import { FailIcon, MergeIcon, NeedsIcon, OkIcon, SparkIcon } from "./icons";
import { useAction } from "./use-action";

function Count({
  icon,
  label,
  count,
  bad,
  to,
}: {
  icon: ReactNode;
  label: string;
  count: number;
  bad?: boolean;
  to: "/" | "/inbox" | "/memory";
}) {
  return (
    <Link
      to={to}
      search={to === "/" ? (label === "Failed runs" ? { filter: "needs" } : {}) : {}}
      className={`recap-row${bad && count > 0 ? " bad" : ""}`}
      data-testid={`recap-${label.toLowerCase().replaceAll(" ", "-")}`}
    >
      {icon}
      {label}
      <span className="n">{count}</span>
    </Link>
  );
}

// Mark seen moves the daemon's mark to now, so the next visit, on this device
// or another, counts from here. It is the card's only action, so it is the
// primary and sits rightmost.
function MarkSeen({ recap }: { recap: RecapResponse }) {
  const queryClient = useQueryClient();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<RecapResponse>("POST", "/api/v1/recap/seen", { idempotency_key }),
    [],
  );
  const onRecorded = useCallback(
    (response: { data: RecapResponse }) =>
      queryClient.setQueryData(queryKeyFor(recapPath), response.data),
    [queryClient],
  );
  // One click per mark: a new mark is a new binding, so it gets a new key.
  const action = useAction<RecapResponse>({
    binding: recap.seen_at ?? "never",
    send,
    onRecorded,
  });
  return (
    <>
      <button
        type="button"
        className="btn recap-mark"
        disabled={action.pending}
        onClick={action.trigger}
      >
        {action.pending ? "Marking..." : "Mark seen"}
      </button>
      <ActionFailure error={action.error} host={undefined} />
    </>
  );
}

// "Since you left", at the top of the Inbox list: what landed, what is newly
// asked, what failed and what memory suggests, counted from the owner's last
// Mark seen. It replaces the command-line recap.
export function RecapCard() {
  const { data: recap } = useQuery(recapQuery());
  if (!recap) return null;
  const since = formatLisbon(recap.since) ?? "your last visit";
  const quiet = recap.landed + recap.decisions + recap.failed_runs + recap.suggestions === 0;
  if (quiet) {
    return (
      <div className="recap quiet" aria-label="Since you left" data-testid="recap-quiet">
        <div className="recap-head">
          <OkIcon />
          <span>Nothing new since {since}</span>
        </div>
      </div>
    );
  }
  return (
    <div className="recap" aria-label="Since you left" data-testid="recap">
      <div className="recap-head">
        <b>Since {since}</b>
        <span className="recap-gap" />
        <MarkSeen recap={recap} />
      </div>
      <Count icon={<MergeIcon />} label="Landed" count={recap.landed} to="/" />
      <Count icon={<NeedsIcon />} label="New decisions" count={recap.decisions} to="/inbox" />
      <Count icon={<FailIcon />} label="Failed runs" count={recap.failed_runs} bad to="/" />
      <Count
        icon={<SparkIcon />}
        label="Memory suggestions"
        count={recap.suggestions}
        to="/memory"
      />
    </div>
  );
}
