import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { TerminalIcon } from "../board/icons";
import { LiveTerminal } from "../board/live-terminal";
import { agentName, useSessionAgent } from "./agent-name";

// View live: the session's own live terminal, the Agents view's expanded
// one, opened over the page and closed back to it. A saved machine's agent
// shows as its read-only tail, as on the card page.
export function ViewLive({ sessionId }: { sessionId: string }) {
  const [open, setOpen] = useState(false);
  const { agent, saved: remote } = useSessionAgent(sessionId);
  const name = agentName(agent);
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
