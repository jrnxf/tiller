import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { machineRowView } from "../../lib/machines";
import { machinesQuery, quotaQuery } from "../../lib/queries";
import { quotaFooterView } from "../../lib/quota";
import type { Machine } from "../../lib/api-types";
import { CheckIcon } from "./icons";

function Pips({ machine }: { machine: Machine }) {
  if (machine.capacity === null || machine.running_agents === null) return null;
  return (
    <span className="pips" aria-hidden="true">
      {Array.from({ length: machine.capacity }, (_, slot) => (
        <i key={slot} className={slot < (machine.running_agents ?? 0) ? "on" : undefined} />
      ))}
    </span>
  );
}

// One row per host. A click filters Board and Agents to that host through the
// `host` search param, which also shows the removable chip in the header.
export function MachinesSection({ activeHost }: { activeHost: string | undefined }) {
  const { data } = useSuspenseQuery(machinesQuery());
  const { data: quota } = useSuspenseQuery(quotaQuery());
  const quotaView = quotaFooterView(quota);
  const quotaLine =
    quotaView?.percentLeft != null
      ? `Claude: ${Math.round(quotaView.percentLeft)}% of this week left${quotaView.resets ? `, ${quotaView.resets}` : ""}.`
      : null;

  return (
    <>
      <div className="grp">
        <h4 id="machines-heading">Machines</h4>
        <span className="sp" />
        {activeHost ? (
          <Link
            to="."
            search={(prev) => ({ ...prev, host: undefined })}
            className="lnk"
            data-testid="machines-clear"
          >
            Clear filter
          </Link>
        ) : null}
      </div>
      <section aria-labelledby="machines-heading" className="machines">
        {data.machines.map((machine) => {
          const view = machineRowView(machine);
          const active = activeHost === machine.id;
          const tipId = `tip-${machine.id}`;
          return (
            <Link
              key={machine.id}
              to="."
              search={(prev) => ({ ...prev, host: active ? undefined : machine.id })}
              className="mach"
              aria-pressed={active}
              aria-describedby={tipId}
              data-testid={`machine-${machine.label}`}
            >
              <span className="mh1">
                <span
                  className={`live${view.tone === "green" ? "" : view.tone === "red" ? " bad" : " warn"}`}
                />
                <b>{machine.label}</b>
                {view.viewOnly ? <em className="mtag">view only</em> : null}
                {active ? (
                  <span className="mfilt">
                    <CheckIcon />
                  </span>
                ) : null}
              </span>
              {view.stale ? (
                <span className="mh2 stale">{view.staleText}</span>
              ) : view.summary ? (
                <span className="mh2">
                  <Pips machine={machine} />
                  <span>{view.summary}</span>
                </span>
              ) : null}
              <span className="tip" role="tooltip" id={tipId}>
                <b>{machine.label}</b>
                {view.stale
                  ? `${view.staleText}. ${view.tooltip ? `${view.tooltip}.` : ""} Cards on this machine show their last known state.`
                  : view.viewOnly
                    ? "can be watched and opened in herdr from here. Answers and dispatches stay on that machine."
                    : "runs agents for this board."}
                {quotaLine ? <span className="tq">{quotaLine}</span> : null}
                {view.stale ? null : (
                  <span className="tq">Click to show only this machine on Board and Agents.</span>
                )}
              </span>
            </Link>
          );
        })}
      </section>
    </>
  );
}
