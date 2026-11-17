import { Link } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";

import { ownerOf, projectKey } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { SearchIcon } from "../shell/icons";
import { GROUP_LABELS, groupRows } from "./model";
import type { ChatRow } from "./model";
import { stamp } from "./thread-items";

// The orchestrators down the left of the chat page, grouped by what they
// need: Needs you, Working, Idle. `/` jumps to the search.
export function ChatList({ rows, current }: { rows: ChatRow[]; current: string }) {
  const [query, setQuery] = useState("");
  const search = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) return;
      if (shortcutsBlocked(event)) return;
      event.preventDefault();
      search.current?.focus();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
  const groups = groupRows(rows, query);
  return (
    <aside className="chat-side" aria-label="Orchestrators">
      <label className="chat-search">
        <SearchIcon />
        <input
          ref={search}
          value={query}
          placeholder="Search"
          aria-label="Search orchestrators"
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              setQuery("");
              event.currentTarget.blur();
            }
          }}
        />
        <kbd>/</kbd>
      </label>
      <nav className="chat-groups" data-testid="chat-groups">
        {groups.length === 0 ? (
          <div className="chat-side-empty">No project matches.</div>
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
      style={{ "--pc": projectColor(row.index) } as CSSProperties}
    >
      <span className="chat-av">
        {project.name.charAt(0).toUpperCase()}
        {row.group === "working" ? <span className="st work" title="Working" /> : null}
        {row.group === "needs" ? <span className="st need" title="Needs you" /> : null}
      </span>
      <span className="chat-conv-body">
        <span className="r1">
          <span className="name">{project.name}</span>
          <span className="when">{stamp(row.at)}</span>
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
