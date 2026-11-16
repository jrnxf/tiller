import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { runRefusal, runRefusalStep } from "../../lib/machines";
import { machinesQuery } from "../../lib/queries";
import { WarnIcon } from "../board/icons";

// Hosts that refuse every new run (a breached RAM or disk floor, recovery.md
// R16). The host works and its link is live, so nothing else on Health says
// that a task started there will not run.
export function HostRefusals() {
  const machines = useQuery(machinesQuery()).data?.machines ?? [];
  const refusing = machines.filter((machine) => runRefusal(machine) !== null);
  if (refusing.length === 0) return null;
  return (
    <section className="cpanel learning" aria-labelledby="host-refusals-title">
      <div className="ch">
        <div>
          <h3 id="host-refusals-title">Machines</h3>
          <p className="cs">Machines that start no new run.</p>
        </div>
      </div>
      <ul className="ln-list">
        {refusing.map((machine) => (
          <li key={machine.id} data-kind="refuses" data-testid={`health-refuses-${machine.label}`}>
            <span className="si bad">
              <WarnIcon />
            </span>
            <span className="t">{machine.label} refuses new runs</span>
            <span className="m">
              {`${runRefusal(machine)}. Cards queued on it wait, and running agents keep running. ${runRefusalStep(machine)}`}
            </span>
            {machine.kind === "local" ? (
              <span className="acts">
                <Link className="btn" to="/settings">
                  Open Settings
                </Link>
              </span>
            ) : null}
          </li>
        ))}
      </ul>
    </section>
  );
}
