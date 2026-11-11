import { useCallback, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import type { DaemonHealth, Machine, PollerHealth } from "../../lib/api-types";
import { doctorQuery, healthQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { RetryIcon } from "../board/icons";
import { checkLines, whenText } from "./check-lines";
import type { CheckLine } from "./check-lines";
import { CardLine, HostCardSection } from "./host-card";

// Run now on a failing poller: it polls at once instead of waiting out its
// backoff, and the line says how that poll went.
function RunNow({
  poller,
  onRan,
}: {
  poller: PollerHealth;
  onRan: (after: PollerHealth | null) => void;
}) {
  const send = useCallback(
    (key: string) =>
      sendMutation<PollerHealth | null>(
        "POST",
        `/api/v1/pollers/${encodeURIComponent(poller.source)}/run`,
        { idempotency_key: key },
      ),
    [poller.source],
  );
  const run = useAction<PollerHealth | null>({
    // A poll that ends moves its last attempt, so the next click is a new one.
    binding: `poller:${poller.source}:${poller.last_attempt_at ?? "never"}`,
    send,
    onRecorded: (response) => onRan(response.data),
  });
  return (
    <>
      <button
        type="button"
        className="btn sm"
        disabled={run.pending}
        aria-busy={run.pending || undefined}
        onClick={run.trigger}
      >
        {run.pending ? "Running" : "Run now"}
      </button>
      {run.error ? (
        <span className="set-line-e" role="alert">
          {run.error.message}
        </span>
      ) : null}
    </>
  );
}

function Line({
  line,
  owner,
  onRan,
}: {
  line: CheckLine;
  owner: boolean;
  onRan: (after: PollerHealth | null) => void;
}) {
  const end =
    line.end?.kind === "run" ? (
      owner ? (
        <RunNow poller={line.end.poller} onRan={onRan} />
      ) : (
        <span className="set-line-r">Owner only</span>
      )
    ) : line.end ? (
      <span className="set-line-r">{line.end.text}</span>
    ) : null;
  return (
    <CardLine
      testid={`check-${line.key}`}
      tone={line.tone}
      title={line.title}
      sub={line.sub}
      detail={line.detail}
      end={end}
    />
  );
}

// The checks of the host serving this board: the ones `tiller doctor` runs,
// run again each time the card opens, and its pollers.
function LocalChecks({ health, owner }: { health: DaemonHealth; owner: boolean }) {
  const client = useQueryClient();
  const doctor = useQuery(doctorQuery());
  // Pollers Run now brought back, shown as fixed while the card is open.
  const [fixed, setFixed] = useState<Record<string, PollerHealth>>({});
  const lines = checkLines(doctor.data, health.pollers ?? [], fixed);
  const ran = useCallback(
    (after: PollerHealth | null) => {
      if (after && after.consecutive_failures === 0) {
        setFixed((before) => ({ ...before, [after.source]: after }));
      }
      void client.invalidateQueries({ queryKey: healthQuery().queryKey });
    },
    [client],
  );
  const again = () => {
    void doctor.refetch();
    void client.invalidateQueries({ queryKey: healthQuery().queryKey });
  };
  const checked = whenText(doctor.data?.checked_at);
  const meta = doctor.isFetching ? "Running the checks" : checked ? `Last run ${checked}` : null;
  return (
    <HostCardSection
      title="Checks"
      meta={meta}
      action={
        <button
          type="button"
          className="btn sm"
          onClick={again}
          disabled={doctor.isFetching}
          aria-busy={doctor.isFetching || undefined}
        >
          <RetryIcon />
          Check again
        </button>
      }
    >
      <ul className="set-lines" aria-live="polite">
        {doctor.isError ? (
          <CardLine
            testid="check-unreadable"
            tone="amber"
            title="Checks"
            detail={[
              `They cannot be run from here: ${doctor.error.message}`,
              " Run tiller doctor in a shell on this host.",
            ]}
          />
        ) : null}
        {lines.map((line) => (
          <Line key={line.key} line={line} owner={owner} onRan={ran} />
        ))}
      </ul>
    </HostCardSection>
  );
}

// A peer's checks run on that host; this board reads only its own. A herdr
// machine has no daemon to check: this host's herdr poller reads it.
function RemoteChecks({
  machine,
  peer,
  board,
}: {
  machine: Machine;
  peer: boolean;
  board: string;
}) {
  return (
    <HostCardSection title="Checks">
      <ul className="set-lines">
        {peer ? (
          <CardLine
            testid="check-elsewhere"
            tone={null}
            title={`Run on ${machine.label}`}
            detail={[
              `Each host checks its own tools and pollers. Open the board ${machine.label} serves to see its checks.`,
            ]}
          />
        ) : (
          <CardLine
            testid="check-elsewhere"
            tone={null}
            title="No daemon to check"
            detail={[
              `${machine.label} runs agents through herdr only. Its herdr poller is in the checks of ${board}.`,
            ]}
          />
        )}
      </ul>
    </HostCardSection>
  );
}

export function HostChecks({
  machine,
  peer,
  board,
  health,
  owner,
}: {
  machine: Machine;
  // A paired Tiller peer, not a herdr-only machine.
  peer: boolean;
  // The label of the host serving this board.
  board: string;
  health: DaemonHealth;
  owner: boolean;
}) {
  return machine.kind === "local" ? (
    <LocalChecks health={health} owner={owner} />
  ) : (
    <RemoteChecks machine={machine} peer={peer} board={board} />
  );
}
