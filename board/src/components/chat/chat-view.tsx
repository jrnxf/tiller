import { useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import type { Editor } from "@tiptap/core";
import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, RefObject } from "react";

import type {
  AgentRow,
  BoardRow,
  Project,
  ThreadContext,
  ThreadItem,
  ThreadRead,
} from "../../lib/api-types";
import { hostField, ownerOf, projectKey } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import {
  agentsQuery,
  attentionQuery,
  boardQuery,
  projectsQuery,
  queryKeyFor,
  repliesPath,
  threadPath,
  threadQuery,
} from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useReplies } from "../attention/replies";
import { ulid } from "../attention/ulid";
import { agentActivity } from "../board/activity";
import { ArrowIcon, TerminalIcon } from "../board/icons";
import { usePhone } from "../board/live-terminal";
import { useNamedProjects } from "../board/peer-state";
import { showToast } from "../board/toast";
import { CloseIcon } from "../shell/icons";
import { Segmented } from "../ui/segmented";
import "../board/board.css";
import "../attention/inbox.css";
import "./chat.css";
import { closeChatDrawer, openChatDrawer } from "./chat-drawer-store";
import { ChatList } from "./chat-list";
import { ChatPrompt, fillEditor } from "./chat-prompt";
import {
  GROUP_LABELS,
  chatRows,
  firstRow,
  firstUnread,
  contextShare,
  queueOf,
  quoteOf,
  suggestionsFor,
  tokens,
  unreadReplies,
  waitingByProject,
  workingStatus,
} from "./model";
import type { ChatGroup, ChatRow } from "./model";
import {
  ActivityBlock,
  AskMessage,
  CheckpointLine,
  DecisionBlock,
  PendingAsk,
  ReplyMessage,
  Stamp,
} from "./thread-items";
import { useChatSend } from "./use-chat-send";

export type ChatVariant = "page" | "drawer";

const VARIANTS = [
  { value: "page", label: "Page" },
  { value: "drawer", label: "Drawer" },
] as const satisfies { value: ChatVariant; label: string }[];

const GROUP_TONE: Record<ChatGroup, string> = { needs: "mute", working: "acc", idle: "mute" };

// The toBottom button shows once the view is this far up from the newest item.
const BOTTOM_SLACK = 120;

// The ring in the header: how full the orchestrator's context is, with the
// numbers and the session's start in its tip.
function ContextRing({ context, since }: { context: ThreadContext; since: string | null }) {
  const tipId = useId();
  const [open, setOpen] = useState(false);
  const share = contextShare(context);
  const radius = 7;
  const length = 2 * Math.PI * radius;
  const used = tokens(context.used);
  const label =
    share === null
      ? `Context: ${used} tokens`
      : `Context: ${Math.round(share * 100)}% used, ${used} of ${tokens(context.window ?? 0)}`;
  return (
    <span className="chat-ctx-wrap" data-open={open}>
      <button
        type="button"
        className="chat-ctx"
        aria-label={label}
        aria-describedby={tipId}
        aria-expanded={open}
        onClick={() => setOpen((shown) => !shown)}
        onBlur={() => setOpen(false)}
        data-testid="chat-context"
      >
        {share === null ? null : (
          <svg width={18} height={18} viewBox="0 0 18 18" aria-hidden="true">
            <circle className="track" cx={9} cy={9} r={radius} />
            <circle
              className="fill"
              cx={9}
              cy={9}
              r={radius}
              strokeDasharray={`${share * length} ${length}`}
              transform="rotate(-90 9 9)"
            />
          </svg>
        )}
        <span>{share === null ? `${used} tokens` : `${Math.round(share * 100)}%`}</span>
      </button>
      <span className="chat-ctx-tip" id={tipId} role="tooltip">
        <span className="row">
          <span>Context</span>
          <b>
            {used}
            {context.window ? ` / ${tokens(context.window)}` : null}
          </b>
        </span>
        {share === null ? null : (
          <span className="bar">
            <span style={{ width: `${share * 100}%` }} />
          </span>
        )}
        {since ? (
          <span className="row">
            <span>Session started</span>
            <b>
              <Stamp at={since} />
            </b>
          </span>
        ) : null}
      </span>
    </span>
  );
}

function ChatHeader({
  project,
  row,
  context,
  since,
  variant,
}: {
  project: Project;
  row: ChatRow;
  context: ThreadContext | null;
  since: string | null;
  variant: ChatVariant;
}) {
  const navigate = useNavigate();
  const owner = ownerOf(project);
  const { orchestrator } = row;
  const view = orchestrator ? agentActivity(orchestrator) : null;
  const switchTo = (next: ChatVariant) => {
    if (next === variant) return;
    if (next === "drawer") {
      openChatDrawer({ project: project.id, owner });
      void navigate({ to: "/", search: ({ host }) => ({ host }) });
    } else {
      closeChatDrawer();
      void navigate({
        to: "/chat/$project",
        params: { project: project.id },
        search: ({ host }) => ({ host, owner }),
      });
    }
  };
  return (
    <header className="chat-top">
      {variant === "page" ? (
        <Link
          to="/chat"
          search={({ host }) => ({ host })}
          className="btn sm icon chat-back"
          aria-label="All chats"
          title="All chats"
        >
          <ArrowIcon to="left" />
        </Link>
      ) : null}
      <span className="chat-title">
        <span className="pdot" style={{ "--pc": projectColor(row.index) } as CSSProperties} />
        <h2>{project.name}</h2>
        <span className={`lab sm ${GROUP_TONE[row.group]}`} data-testid="chat-state">
          {view ? <i className={`ld ${view.dot}${view.dot === "live" ? " pulse" : ""}`} /> : null}
          {orchestrator ? GROUP_LABELS[row.group] : "Not running"}
        </span>
      </span>
      <span className="chat-top-acts">
        {context ? <ContextRing context={context} since={since} /> : null}
        <Segmented
          className="chat-seg"
          label="Show the chat as"
          options={[...VARIANTS]}
          value={variant}
          onChange={switchTo}
        />
        {orchestrator ? (
          <Link
            to="/agents"
            search={({ host }) => ({ host, agent: orchestrator.session_id })}
            className="btn sm"
            title="Open the orchestrator's terminal"
            onClick={variant === "drawer" ? closeChatDrawer : undefined}
          >
            <TerminalIcon />
            <span className="hide-sm">Terminal</span>
          </Link>
        ) : null}
        {variant === "drawer" ? (
          <button
            type="button"
            className="btn sm icon"
            aria-label="Close the chat"
            title="Close"
            onClick={closeChatDrawer}
          >
            <CloseIcon />
          </button>
        ) : null}
      </span>
    </header>
  );
}

// The thread is marked read once per opening, when it shows a reply the
// owner has not read: one call for all of them, never one per reply.
function useMarkRead(project: Project, items: ThreadItem[] | undefined) {
  const queryClient = useQueryClient();
  const done = useRef(false);
  const unread = items ? unreadReplies(items).length : 0;
  useEffect(() => {
    if (done.current || unread === 0) return;
    done.current = true;
    const owner = ownerOf(project);
    sendMutation<ThreadRead>("POST", `/api/v1/projects/${project.id}/thread/read`, {
      idempotency_key: ulid(),
      ...hostField(project),
    }).then(
      () => {
        void queryClient.invalidateQueries({
          queryKey: queryKeyFor(repliesPath(project.id, owner)),
        });
        void queryClient.invalidateQueries({
          queryKey: queryKeyFor(threadPath(project.id, owner)),
        });
      },
      () =>
        showToast("bad", "Could not mark the chat read", "Its replies stay unread in the Inbox."),
    );
  }, [project, queryClient, unread]);
}

// Under the latest reply, when nothing was sent after it.
function followAt(items: ThreadItem[]): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item?.type === "reply") return index;
    if (item?.type === "ask") return -1;
  }
  return -1;
}

function ThreadBody({
  project,
  row,
  editorRef,
  pending,
}: {
  project: Project;
  row: ChatRow;
  editorRef: RefObject<Editor | null>;
  // The message on its way, shown under the thread until the thread holds it.
  pending: string | null;
}) {
  const queryClient = useQueryClient();
  const owner = ownerOf(project);
  const thread = useQuery(threadQuery(project.id, owner));
  const attention = useQuery(attentionQuery()).data?.items ?? [];
  const [older, setOlder] = useState<{ items: ThreadItem[]; before: number | null } | null>(null);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);
  const [away, setAway] = useState(false);
  const stuck = useRef(true);
  const lastTop = useRef(0);
  const newLine = useRef<HTMLDivElement>(null);
  const jumped = useRef(false);

  const page = thread.data?.items;
  // Both are fixed at the first read, before the chat marks its replies read:
  // where the "New" line goes, and the newest item then, after which an item
  // came in while the chat was open and eases in.
  const [opened, setOpened] = useState<{ newFrom: number | null; last: number } | null>(null);
  if (opened === null && page) {
    setOpened({ newFrom: firstUnread(page), last: page.at(-1)?.seq ?? 0 });
  }
  useMarkRead(project, page);
  // A page read earlier keeps only what the newest page no longer holds.
  const first = page?.[0]?.seq ?? Number.POSITIVE_INFINITY;
  const items = [...(older?.items ?? []).filter((item) => item.seq < first), ...(page ?? [])];
  const before = older ? older.before : (thread.data?.next_before ?? null);
  const last = items.at(-1);
  const view = row.orchestrator ? agentActivity(row.orchestrator) : null;
  const shownPending = pending !== null && !(last?.type === "ask" && last.text === pending);
  const working = view?.dot === "live" && last?.type === "ask" && !shownPending;
  const status = working && last?.type === "ask" ? workingStatus(row.orchestrator, last.at) : null;
  const follow = followAt(items);
  const fresh = (seq: number) => opened !== null && seq > opened.last;

  // A message sent from the box shows at the bottom, so the view goes there.
  useEffect(() => {
    if (pending !== null) stuck.current = true;
  }, [pending]);

  // A chat opened with new replies starts at the "New" line, unless the
  // newest item is in view from there anyway.
  useLayoutEffect(() => {
    const element = scroller.current;
    const line = newLine.current;
    if (jumped.current || !element || !line) return;
    jumped.current = true;
    const top = Math.max(0, line.offsetTop - 16);
    if (top >= element.scrollHeight - element.clientHeight) return;
    element.scrollTop = top;
    lastTop.current = top;
    stuck.current = false;
  });
  // The newest item stays in view while the owner reads at the bottom: after
  // every render, as a new item, a step or an opened block can each grow it.
  useLayoutEffect(() => {
    const element = scroller.current;
    if (element && stuck.current) element.scrollTop = element.scrollHeight;
  });
  // And when a size changes with no render here: the box below grows as its
  // editor loads, the window resizes, a font arrives.
  useEffect(() => {
    const element = scroller.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (stuck.current) element.scrollTop = element.scrollHeight;
    });
    observer.observe(element);
    if (element.firstElementChild) observer.observe(element.firstElementChild);
    return () => observer.disconnect();
  }, []);

  // Only a move up lets go of the bottom: a scroll event of the view's own
  // can land after the box below has grown, with a gap it did not make.
  const onScroll = () => {
    const element = scroller.current;
    if (!element) return;
    const gap = element.scrollHeight - element.scrollTop - element.clientHeight;
    const up = element.scrollTop < lastTop.current;
    lastTop.current = element.scrollTop;
    if (gap < 8) stuck.current = true;
    else if (up) stuck.current = false;
    setAway(gap > BOTTOM_SLACK);
  };

  const showOlder = () => {
    if (before === null || loadingOlder) return;
    setLoadingOlder(true);
    const element = scroller.current;
    const height = element?.scrollHeight ?? 0;
    queryClient.fetchQuery(threadQuery(project.id, owner, before)).then(
      (response) => {
        setOlder((kept) => ({
          items: [...response.items, ...(kept?.items ?? [])],
          before: response.next_before ?? null,
        }));
        setLoadingOlder(false);
        // The items in view stay where they were.
        requestAnimationFrame(() => {
          if (element) element.scrollTop += element.scrollHeight - height;
        });
      },
      () => {
        setLoadingOlder(false);
        showToast("bad", "Could not read earlier messages", "Try again in a moment.");
      },
    );
  };

  const toBottom = () => {
    const element = scroller.current;
    if (!element) return;
    stuck.current = true;
    element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
  };

  let body;
  if (thread.isPending) {
    body = <div className="chat-note">Reading the chat…</div>;
  } else if (thread.isError && !page) {
    body = (
      <div className="chat-note" role="alert">
        <b>The chat could not be read</b>
        {thread.error.message}
        <button type="button" className="btn sm" onClick={() => void thread.refetch()}>
          Try again
        </button>
      </div>
    );
  } else if (items.length === 0 && !shownPending) {
    body = (
      <div className="chat-note" data-testid="chat-empty">
        <b>No messages yet</b>
        {row.orchestrator
          ? `Send ${project.name}'s orchestrator a message to start.`
          : "Sending a message starts the orchestrator."}
      </div>
    );
  } else {
    body = (
      <>
        {before !== null ? (
          <button
            type="button"
            className="btn sm chat-older"
            onClick={showOlder}
            disabled={loadingOlder}
          >
            {loadingOlder ? "Reading…" : "Show earlier messages"}
          </button>
        ) : null}
        {items.map((item, index) => {
          const line =
            item.seq === opened?.newFrom ? (
              <div className="chat-new" ref={newLine} role="separator" data-testid="chat-new">
                <span>New</span>
              </div>
            ) : null;
          switch (item.type) {
            case "ask":
              return <AskMessage key={item.seq} ask={item} owner={owner} fresh={fresh(item.seq)} />;
            case "activity":
              return (
                <ActivityBlock
                  key={item.seq}
                  activity={item}
                  owner={owner}
                  fresh={fresh(item.seq)}
                />
              );
            case "reply":
              return (
                <Fragment key={item.seq}>
                  {line}
                  <ReplyMessage
                    item={item}
                    owner={owner}
                    fresh={fresh(item.seq)}
                    follow={index === follow ? suggestionsFor(row.group) : []}
                    onReply={(text) => fillEditor(editorRef.current, quoteOf(text))}
                    onSuggest={(text) => fillEditor(editorRef.current, text)}
                  />
                </Fragment>
              );
            case "decision":
              return (
                <DecisionBlock key={item.seq} decision={item} attention={attention} owner={owner} />
              );
            case "checkpoint":
              // A first start says nothing the thread's top does not: only a
              // restart, where the orchestrator lost what is above, gets a line.
              return item.restarted ? <CheckpointLine key={item.seq} checkpoint={item} /> : null;
          }
        })}
        {shownPending ? <PendingAsk text={pending} /> : null}
        {working ? (
          <div className="chat-working" data-testid="chat-working">
            <span className="chat-spin" aria-hidden="true" />
            Working
            {status ? <span className="st">{status}</span> : null}
          </div>
        ) : null}
      </>
    );
  }

  return (
    <div className="chat-scroll" ref={scroller} onScroll={onScroll}>
      <div className="chat-thread" aria-live="polite" aria-busy={thread.isFetching}>
        {body}
      </div>
      {away ? (
        <button
          type="button"
          className="chat-bottom"
          aria-label="Scroll to the newest"
          onClick={toBottom}
        >
          <ArrowIcon to="down" />
        </button>
      ) : null}
    </div>
  );
}

// The chat list's rows and the reads they come from, which the page, its
// list and the drawer share.
function useChatRows() {
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data).projects;
  const rows = useQuery(boardQuery()).data?.rows ?? [];
  const orchestrators: AgentRow[] = useQuery(agentsQuery()).data?.orchestrators ?? [];
  const attention = useQuery(attentionQuery()).data?.items ?? [];
  const { replies } = useReplies(projects);
  const list = chatRows(projects, orchestrators, waitingByProject(attention, rows), replies);
  return { list, rows };
}

// /chat: on a wide screen, the chat at the top of the list opens at once. On
// a phone the list is the page, and a project opens its chat.
export function ChatHome() {
  const navigate = useNavigate();
  const phone = usePhone();
  const { list } = useChatRows();
  const first = phone ? null : firstRow(list);
  useEffect(() => {
    if (!first) return;
    void navigate({
      to: "/chat/$project",
      params: { project: first.project.id },
      search: ({ host }) => ({ host, owner: ownerOf(first.project) }),
      replace: true,
    });
  }, [first, navigate]);
  return (
    <div className="chat" data-variant="page" data-home="" data-testid="chat-home">
      <ChatList rows={list} current="" />
      <div className="chat-main">
        {list.length === 0 ? (
          <div className="chat-note" data-testid="chat-empty">
            <b>No projects yet</b>
            Add a project, and its orchestrator's chat shows here.
          </div>
        ) : null}
      </div>
    </div>
  );
}

// The chat with one project's orchestrator: the page at /chat/$project, or
// the drawer over the board. One component, so both behave alike.
export function ChatView({
  projectId,
  owner,
  variant,
}: {
  projectId: string;
  owner: string | undefined;
  variant: ChatVariant;
}) {
  const { list, rows } = useChatRows();
  const row = list.find(
    (entry) => entry.project.id === projectId && ownerOf(entry.project) === owner,
  );

  if (!row) {
    return (
      <div className="chat" data-variant={variant}>
        {variant === "page" ? <ChatList rows={list} current="" /> : null}
        <div className="chat-main">
          <div className="chat-note" data-testid="chat-missing">
            <b>This project is not on the board</b>
            It may have been removed or archived.
          </div>
        </div>
      </div>
    );
  }
  return (
    <div className="chat" data-variant={variant} data-testid={`chat-${variant}`}>
      {variant === "page" ? <ChatList rows={list} current={projectKey(row.project)} /> : null}
      <ChatMain key={projectKey(row.project)} row={row} rows={rows} variant={variant} />
    </div>
  );
}

// One project's chat: its header, the thread and the box.
function ChatMain({
  row,
  rows,
  variant,
}: {
  row: ChatRow;
  rows: BoardRow[];
  variant: ChatVariant;
}) {
  const { project } = row;
  const owner = ownerOf(project);
  const editorRef = useRef<Editor | null>(null);
  const { pending, send } = useChatSend(project, row.orchestrator !== null);
  const thread = useQuery(threadQuery(project.id, owner)).data;
  const checkpoints = (thread?.items ?? []).filter((item) => item.type === "checkpoint");
  const since = checkpoints.at(-1)?.at ?? null;
  const queue = queueOf(project, rows, row.orchestrator);
  const projectRows = rows.filter(
    (card) => card.project_id === project.id && ownerOf(card) === owner,
  );

  return (
    <div className="chat-main">
      <ChatHeader
        project={project}
        row={row}
        context={thread?.context ?? null}
        since={since}
        variant={variant}
      />
      <ThreadBody project={project} row={row} editorRef={editorRef} pending={pending} />
      <div className="chat-dock">
        <div className="chat-dock-in">
          <ChatPrompt
            project={project}
            index={row.index}
            orchestrator={row.orchestrator}
            working={row.group === "working"}
            rows={projectRows}
            queue={queue}
            editorRef={editorRef}
            pending={pending}
            send={send}
          />
        </div>
      </div>
    </div>
  );
}
