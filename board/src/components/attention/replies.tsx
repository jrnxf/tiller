import { useQueries, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useCallback } from "react";
import type { CSSProperties, ReactNode } from "react";

import type { MutationResponse, Project, RepliesResponse, ReplyItem } from "../../lib/api-types";
import { isPeerRow, projectIndex } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { queryKeyFor, repliesPath, repliesQuery } from "../../lib/queries";
import { askPrefillStore } from "../shell/ask-prefill";
import { CheckIcon } from "../shell/icons";
import { ActionFailure } from "./action-failure";
import { sendMutation } from "./api";
import { formatAge, formatWhen } from "./format";
import { ReplyIcon } from "./icons";
import { useAction } from "./use-action";

// The unread replies of this host's shown projects, newest first. A peer's
// project is left out: replies are never federated, and a peer's stay there.
// A project whose read fails shows nothing, and the rest still show.
// `loading` holds while a first read is still on its way: the boot snapshot
// does not carry replies, so a link to one waits for it.
export function useReplies(projects: Project[]): { replies: ReplyItem[]; loading: boolean } {
  const local = projects.filter((project) => !isPeerRow(project));
  const results = useQueries({
    queries: local.map((project) => repliesQuery(project.id)),
  });
  const replies = results.flatMap((result) => result.data?.replies ?? []);
  replies.sort((a, b) => b.replied_at.localeCompare(a.replied_at));
  return { replies, loading: results.some((result) => result.isPending) };
}

function ago(iso: string): string {
  const age = formatAge(iso);
  return age === "just now" || age === "unknown" ? age : `${age} ago`;
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
  const index = projectIndex(projects, { project_id: reply.project });
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

function ReplyRow({
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
      className="irow"
      data-selected={selected}
      data-testid={`inbox-row-${reply.id}`}
    >
      <span className="ii reply-ii">
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
          replied {ago(reply.replied_at)}
        </span>
      </span>
    </Link>
  );
}

export function RepliesGroup({
  replies,
  selected,
  projects,
}: {
  replies: ReplyItem[];
  selected: string | undefined;
  projects: Project[];
}) {
  if (replies.length === 0) return null;
  return (
    <section aria-label="Replies">
      <div className="ghead reply">
        <ReplyIcon />
        Replies<span className="c">{replies.length}</span>
        <span className="sp" />
        <span className="why">the orchestrator answered</span>
      </div>
      {replies.map((reply) => (
        <ReplyRow
          key={reply.id}
          reply={reply}
          selected={reply.id === selected}
          projects={projects}
        />
      ))}
    </section>
  );
}

// Mark read takes the reply off the Inbox, on this device and every other.
// It is the detail's main action, so it is the primary and sits rightmost.
function MarkRead({ reply }: { reply: ReplyItem }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<ReplyItem>("POST", `/api/v1/replies/${reply.id}/read`, { idempotency_key }),
    [reply.id],
  );
  const onRecorded = useCallback(
    (_response: MutationResponse<ReplyItem>) => {
      queryClient.setQueryData<RepliesResponse | undefined>(
        queryKeyFor(repliesPath(reply.project)),
        (old) => (old ? { replies: old.replies.filter((other) => other.id !== reply.id) } : old),
      );
      // The reply is gone from the list, so the Inbox shows what leads it.
      void navigate({ to: "/inbox", search: ({ host }) => ({ host }) });
    },
    [queryClient, navigate, reply.id, reply.project],
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
      <ActionFailure error={action.error} host={undefined} />
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
          replied {formatWhen(reply.replied_at)} - {ago(reply.replied_at)}
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
        <button
          type="button"
          className="btn"
          onClick={() => askPrefillStore.set({ project: reply.project, text: reply.question })}
        >
          Ask again
        </button>
        <MarkRead reply={reply} />
      </div>
    </>
  );
}
