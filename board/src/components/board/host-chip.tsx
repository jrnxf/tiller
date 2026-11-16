import { serverNow } from "../../lib/clock";
import { runRefusal } from "../../lib/machines";
import { asOfText, VIEW_ONLY_REASON } from "../../lib/peers";
import type { Owned, Standing } from "../../lib/peers";
import { ServerIcon } from "../shell/icons";
import { useTick } from "./ago";
import { useFederated, useHostLabel, useMachineRows, useStanding } from "./peer-state";

// The host that owns a row. Once any peer is paired it shows on a row only
// when that host needs the owner's eye (its link is down, it is paired view
// only, it refuses runs or its health is not ok), or when `always` asks for
// it, as the card page does for the card's details. A host with no peer never
// shows it.
export function HostChip({ entity, always = false }: { entity: Owned; always?: boolean }) {
  const federated = useFederated();
  const label = useHostLabel(entity);
  const standing = useStanding(entity);
  const machine = useMachineRows().find((row) => row.label === label);
  if (!federated || !label) return null;
  const refusal = machine ? runRefusal(machine) : null;
  const needsEye =
    standing.dim || !standing.canAct || refusal !== null || (machine && machine.health !== "ok");
  if (!always && !needsEye) return null;
  return (
    <span
      className={`hchip${standing.dim ? " off" : ""}`}
      title={standing.reason ?? refusal ?? `Owned by ${label}`}
      data-testid="host-chip"
    >
      <ServerIcon />
      {label}
    </span>
  );
}

// "as of Tue 14:02, 6 min ago", counting on while the link stays down.
export function AsOf({ asOf }: { asOf: string | null }) {
  useTick();
  return <span className="asof">{asOfText(asOf, serverNow())}</span>;
}

// The line a screen shows for rows of a peer that is not live: what it is
// showing and why nothing can be changed.
export function PeerNote({ standing }: { standing: Standing }) {
  if (!standing.peer || (standing.live && standing.canAct)) return null;
  return (
    <p className="peer-note" data-testid="peer-note" role="note">
      {standing.dim ? (
        <>
          <AsOf asOf={standing.asOf} />
          {". "}
        </>
      ) : null}
      {standing.reason === VIEW_ONLY_REASON
        ? `${standing.host} is paired view only, so make changes from that host.`
        : `${standing.reason}.`}
    </p>
  );
}
