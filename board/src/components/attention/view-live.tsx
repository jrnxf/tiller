import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { TerminalIcon } from "../board/icons";
import { LiveTerminal } from "../board/live-terminal";
import { ownerOf } from "../../lib/peers";
import { agentTitle, useSessionAgent } from "./agent-name";

// View live: the session's own live terminal, the Agents view's expanded
// one, opened over the page and closed back to it.
export function ViewLive({ sessionId }: { sessionId: string }) {
  const [open, setOpen] = useState(false);
  const { agent } = useSessionAgent(sessionId);
  const name = agentTitle(agent);
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
          heading={<span className="mir-title">{name}</span>}
          actions={
            <Link
              to="/agents"
              search={({ host }) => ({ host, agent: sessionId })}
              className="btn sm"
            >
              View in Agents
            </Link>
          }
          peer={agent ? ownerOf(agent) : undefined}
          onClose={() => setOpen(false)}
        />
      ) : null}
    </>
  );
}
