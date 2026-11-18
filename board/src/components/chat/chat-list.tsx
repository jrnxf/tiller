import { Link } from "@tanstack/react-router";

import { ownerOf, projectKey } from "../../lib/peers";
import { GROUP_LABELS, groupRows } from "./model";
import type { ChatRow } from "./model";
import { Stamp } from "./thread-items";

// The orchestrators down the left of the chat page, grouped by what they
// need: Needs you, Working, Idle.
export function ChatList({ rows, current }: { rows: ChatRow[]; current: string }) {
  const groups = groupRows(rows);
  return (
    <aside className="chat-side" aria-label="Orchestrators">
      <nav className="chat-groups" data-testid="chat-groups">
        {groups.length === 0 ? (
          <div className="chat-side-empty">No projects yet.</div>
        ) : (
          groups.map(({ group, rows: members }) => (
            <div key={group} data-group={group}>
              <div className="chat-glabel">
                <span>{GROUP_LABELS[group]}</span>
                <span className="n">{members.length}</span>
              </div>
              <ul className="chat-convs">
                {members.map((row) => (
                  <li key={projectKey(row.project)}>
                    <ChatListRow row={row} current={projectKey(row.project) === current} />
                  </li>
                ))}
              </ul>
            </div>
          ))
        )}
      </nav>
    </aside>
  );
}

function ChatListRow({ row, current }: { row: ChatRow; current: boolean }) {
  const { project } = row;
  return (
    <Link
      to="/chat/$project"
      params={{ project: project.id }}
      search={({ host }) => ({ host, owner: ownerOf(project) })}
      className={`chat-conv${row.unread > 0 ? " unread" : ""}`}
      aria-current={current ? "page" : undefined}
    >
      <span className="chat-av">
        {project.name.charAt(0).toUpperCase()}
        {row.group === "working" ? <span className="st work" title="Working" /> : null}
        {row.group === "needs" ? <span className="st need" title="Needs you" /> : null}
      </span>
      <span className="chat-conv-body">
        <span className="r1">
          <span className="name">{project.name}</span>
          <Stamp className="when" at={row.at} />
        </span>
        <span className="r2">
          <span className="snip">{row.snippet}</span>
          {row.waiting > 0 ? (
            <span className="cnt" aria-label={`${row.waiting} waiting on you`}>
              {row.waiting}
            </span>
          ) : null}
        </span>
      </span>
    </Link>
  );
}
