import { serverNow } from "../../lib/clock";
import { asOfText } from "../../lib/peers";
import type { Owned, Standing } from "../../lib/peers";
import { ServerIcon } from "../shell/icons";
import { useTick } from "./ago";
import { useFederated, useHostLabel, useStanding } from "./peer-state";

// The host that owns a row. It shows on every row once any peer is paired,
// and not at all on a host with none.
export function HostChip({ entity }: { entity: Owned }) {
  const federated = useFederated();
  const label = useHostLabel(entity);
  const standing = useStanding(entity);
  if (!federated || !label) return null;
  return (
    <span
      className={`hchip${standing.dim ? " off" : ""}`}
      title={standing.reason ?? `Owned by ${label}`}
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
      {standing.reason}.
    </p>
  );
}
