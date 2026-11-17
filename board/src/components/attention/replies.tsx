import { useQueries, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useCallback } from "react";
import type { CSSProperties, ReactNode } from "react";

import type { MutationResponse, Project, RepliesResponse, ReplyItem } from "../../lib/api-types";
import { hostField, ownerKey, ownerOf, projectIndex } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { queryKeyFor, repliesPath, repliesQuery } from "../../lib/queries";
import { askPrefillStore } from "../shell/ask-prefill";
import { CheckIcon } from "../shell/icons";
import { ActionFailure } from "./action-failure";
import { sendMutation } from "./api";
import { formatAgo, formatWhen } from "./format";
import { ReplyIcon } from "./icons";
import { useAction } from "./use-action";

// The unread replies of the shown projects, newest first. A peer's come from
// what its `act` link carries (protocol.md section 10), each tagged with the
// project's host and link, so it reads, dims and is marked read as that
// peer's. A project whose read fails shows nothing, and the rest still show.
// `loading` holds while a first read is still on its way: the boot snapshot
// does not carry replies, so a link to one waits for it.
export function useReplies(projects: Project[]): { replies: ReplyItem[]; loading: boolean } {
  const results = useQueries({
    queries: projects.map((project) => repliesQuery(project.id, ownerOf(project))),
  });
  const replies = results.flatMap((result, index) => {
    const project = projects[index];
    const owner = project ? ownerOf(project) : undefined;
    const rows = result.data?.replies ?? [];
    return owner && project
      ? rows.map((reply) => ({ ...reply, host: owner, link: project.link, as_of: project.as_of }))
      : rows;
  });
  replies.sort((a, b) => b.replied_at.localeCompare(a.replied_at));
  return { replies, loading: results.some((result) => result.isPending) };
}

// The project dot and name a reply belongs to, when the list still has it.
function ProjectOf({
  reply,
  projects,
  sep,
}: {
  reply: ReplyItem;
  projects: Project[];
  sep: string;
}) {
  const index = projectIndex(projects, { ...reply, project_id: reply.project });
  const project = projects[index];
  if (!project) return null;
  return (
    <>
      <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
      {project.name}
      <span className={sep}>/</span>
    </>
  );
}

// A reply in the Inbox list. Every listed reply is unread: Mark read takes it
// off the list.
export function ReplyRow({
  reply,
  selected,
  projects,
}: {
  reply: ReplyItem;
  selected: boolean;
  projects: Project[];
}) {
  return (
    <Link
      to="/inbox"
      search={({ host }) => ({ host, item: reply.id })}
      className="irow unread"
      data-selected={selected}
      data-testid={`inbox-row-${reply.id}`}
    >
      <span className="ii kind reply" title="Reply">
        <ReplyIcon />
      </span>
      <span className="bd">
        <span className="t">{reply.question}</span>
        <span className="s">
          <ProjectOf reply={reply} projects={projects} sep="sl" />
          {reply.card ? (
            <>
              {reply.card.display_id}
              <span className="sl">/</span>
            </>
          ) : null}
          reply<span className="sl">/</span>
          {formatAgo(reply.replied_at)}
        </span>
      </span>
      <span className="udot" role="img" aria-label="Unread" />
    </Link>
  );
}

// Mark read takes the reply off the Inbox, on this device and every other.
// It is the detail's main action, so it is the primary and sits rightmost.
function MarkRead({ reply }: { reply: ReplyItem }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<ReplyItem>("POST", `/api/v1/replies/${reply.id}/read`, {
        idempotency_key,
        ...hostField(reply),
      }),
    [reply],
  );
  const onRecorded = useCallback(
    (_response: MutationResponse<ReplyItem>) => {
      queryClient.setQueryData<RepliesResponse | undefined>(
        queryKeyFor(repliesPath(reply.project, ownerOf(reply))),
        (old) => (old ? { replies: old.replies.filter((other) => other.id !== reply.id) } : old),
      );
      // The reply is gone from the list, so the Inbox shows what leads it.
      void navigate({ to: "/inbox", search: ({ host }) => ({ host }) });
    },
    [queryClient, navigate, reply],
  );
  const action = useAction<ReplyItem>({ binding: reply.id, send, onRecorded });
  return (
    <>
      <button
        type="button"
        className="btn primary"
        disabled={action.pending}
        onClick={action.trigger}
        data-testid="reply-mark-read"
      >
        <CheckIcon />
        {action.pending ? "Marking..." : "Mark read"}
      </button>
      <ActionFailure error={action.error} host={ownerOf(reply)} />
    </>
  );
}

export function ReplyDetail({
  reply,
  projects,
  back,
}: {
  reply: ReplyItem;
  projects: Project[];
  back: ReactNode;
}) {
  const card = reply.card;
  return (
    <>
      {back}
      <div className="d-meta">
        <span className="lab sm">
          <ReplyIcon />
          Reply
        </span>
        <ProjectOf reply={reply} projects={projects} sep="sep" />
        <span>
          replied {formatWhen(reply.replied_at)} - {formatAgo(reply.replied_at)}
        </span>
      </div>
      <div className="blk reply-blk">
        <div className="blk-h">
          {reply.to === "comment" ? "You commented" : "You asked"}
          <span className="hint">{formatWhen(reply.asked_at)}</span>
        </div>
        <p className="reply-q" data-testid="reply-question">
          {reply.question}
        </p>
      </div>
      <div className="blk reply-blk">
        <div className="blk-h">
          Orchestrator replied
          <span className="hint">{formatWhen(reply.replied_at)}</span>
        </div>
        <div className="reply-a" data-testid="reply-body">
          {reply.body}
        </div>
      </div>
      {card ? (
        <div className="blk reply-blk">
          <div className="blk-h">Card</div>
          <Link
            to="/cards/$cardId"
            params={{ cardId: card.display_id }}
            search={{ owner: ownerOf(reply) }}
            className="reply-card"
            data-testid="reply-card"
          >
            <span className="id">{card.display_id}</span>
            <span className="ct">{card.title}</span>
            {card.relation === "created" ? <span className="lab sm">new card</span> : null}
          </Link>
        </div>
      ) : null}
      <div className="acts reply-acts">
        <Link
          to="/chat/$project"
          params={{ project: reply.project }}
          search={({ host }) => ({ host, owner: ownerOf(reply) })}
          className="btn"
          data-testid="reply-chat"
        >
          Open the chat
        </Link>
        <button
          type="button"
          className="btn"
          onClick={() =>
            askPrefillStore.set({ project: ownerKey(reply.project, reply), text: reply.question })
          }
        >
          Ask again
        </button>
        <MarkRead reply={reply} />
      </div>
    </>
  );
}
