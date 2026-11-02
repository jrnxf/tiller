import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { machineRowView } from "../../lib/machines";
import { machinesQuery } from "../../lib/queries";

// One row per host. A click filters Board and Agents to that host through the
// `host` search param, which also shows the removable chip in the header.
export function MachinesSection({ activeHost }: { activeHost: string | undefined }) {
  const { data } = useSuspenseQuery(machinesQuery());

  return (
    <section className="sec" aria-labelledby="machines-heading">
      <h4 id="machines-heading">Machines</h4>
      {data.machines.map((machine) => {
        const view = machineRowView(machine);
        const active = activeHost === machine.id;
        return (
          <Link
            key={machine.id}
            to="."
            search={(prev) => ({ ...prev, host: active ? undefined : machine.id })}
            className={`row machine${active ? " on" : ""}`}
            title={view.tooltip ?? undefined}
            aria-pressed={active}
            data-testid={`machine-${machine.label}`}
          >
            <span className={`dot ${view.tone}`} />
            <span className="machine-body">
              <span className="machine-name">
                {machine.label}
                {view.viewOnly ? <span className="tag">view only</span> : null}
              </span>
              {view.stale ? (
                <span className="machine-sub stale">{view.staleText}</span>
              ) : view.summary ? (
                <span className="machine-sub">{view.summary}</span>
              ) : null}
            </span>
          </Link>
        );
      })}
    </section>
  );
}
