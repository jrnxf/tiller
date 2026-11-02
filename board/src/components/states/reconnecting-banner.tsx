import { useSuspenseQuery } from "@tanstack/react-query";

import { connectionStore } from "../../lib/connection";
import { machinesQuery } from "../../lib/queries";
import { useStore } from "../../lib/store";
import { RefreshIcon } from "../shell/icons";
import { machineUnreachable } from "./degraded";

// Shown while the board cannot reach the daemon, or while the daemon cannot
// reach the local herdr (recovery.md R18). Both leave the last known state on
// screen and queue answers.
export function ReconnectingBanner() {
  const connection = useStore(connectionStore);
  const { data } = useSuspenseQuery(machinesQuery());
  const herdrDown = data.machines.some(
    (machine) => machine.kind === "local" && machineUnreachable(machine),
  );
  const daemonDown = connection === "reconnecting";
  const show = daemonDown || herdrDown;
  return (
    <div
      className="banner warn"
      role="status"
      aria-live="polite"
      hidden={!show}
      data-testid="degraded-pill"
    >
      <RefreshIcon />
      <div className="tx">
        <b>{daemonDown ? "Daemon reconnecting." : "herdr reconnecting."}</b> Showing the last known
        state; answers are queued until it is back.
      </div>
    </div>
  );
}
