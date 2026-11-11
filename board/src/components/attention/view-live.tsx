import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { agentsQuery } from "../../lib/queries";
import { TerminalIcon } from "../board/icons";
import { LiveTerminal } from "../board/live-terminal";
import { useMachineRows } from "../board/peer-state";

// View live: the session's own live terminal, the Agents view's expanded
// one, opened over the page and closed back to it. A saved machine's agent
// shows as its read-only tail, as on the card page.
export function ViewLive({ sessionId }: { sessionId: string }) {
  const [open, setOpen] = useState(false);
  const agents = useQuery(agentsQuery()).data?.orchestrators ?? [];
  const machines = useMachineRows();
  const agent = agents
    .flatMap((row) => [row, ...row.workers])
    .find((row) => row.session_id === sessionId);
  const machine = agent ? machines.find((row) => row.id === agent.host_id) : undefined;
  const remote = machine?.kind === "remote" ? machine.label : undefined;
  const name = agent?.herdr_name ?? agent?.role ?? "Agent";
  return (
    <>
      <button
        type="button"
        className="btn sm primary"
        title="Open this agent's live terminal here"
        onClick={() => setOpen(true)}
      >
        <TerminalIcon />
        View live
      </button>
      {open ? (
        <LiveTerminal
          variant="popup"
          sessionId={sessionId}
          label={name}
          heading={
            <span className="mir-title">
              {name}
              {remote ? <span className="meta">{remote}</span> : null}
            </span>
          }
          actions={
            <Link
              to="/agents"
              search={({ host }) => ({ host, agent: sessionId })}
              className="btn sm"
            >
              View in Agents
            </Link>
          }
          machine={remote}
          onClose={() => setOpen(false)}
        />
      ) : null}
    </>
  );
}
