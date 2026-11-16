import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { runRefusal, runRefusalStep } from "../../lib/machines";
import { machinesQuery } from "../../lib/queries";
import { WarnIcon } from "../shell/icons";

// Shown on every screen while a host refuses every new run (a breached RAM
// or disk floor, recovery.md R16). The host works and its link is live, so
// without this a task filed there waits with no word of why.
export function HostRefusalBanner() {
  const machines = useQuery(machinesQuery()).data?.machines ?? [];
  const banners = machines.map((machine) => {
    const why = runRefusal(machine);
    if (!why) return null;
    return (
      <div
        key={machine.id}
        className="banner warn"
        role="status"
        aria-live="polite"
        data-testid={`refusal-banner-${machine.label}`}
      >
        <WarnIcon />
        <div className="tx">
          <b>{machine.label} refuses new runs.</b> {why}. {runRefusalStep(machine)}
        </div>
        {machine.kind === "local" ? (
          <Link className="btn sm" to="/settings">
            Settings
          </Link>
        ) : null}
      </div>
    );
  });
  return <>{banners}</>;
}
