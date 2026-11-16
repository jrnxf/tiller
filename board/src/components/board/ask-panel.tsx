import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useCallback, useMemo } from "react";
import type { CSSProperties } from "react";

import type {
  AskDelivery,
  AskDetail,
  AskDisposition,
  BoardResponse,
  MutationResponse,
  ReplyItem,
} from "../../lib/api-types";
import { ownerKey, ownerOf, projectIndex } from "../../lib/peers";
import type { Owned } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { askQuery, boardPath, projectsQuery, queryKeyFor } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { sendMutation } from "../attention/api";
import { formatAgo, formatWhen } from "../attention/format";
import { useAction } from "../attention/use-action";
import "../attention/inbox.css";
import { CloseIcon } from "../shell/icons";
import { askPrefillStore } from "../shell/ask-prefill";
import { HostChip, PeerNote } from "./host-chip";
import { ChevronRightIcon } from "./icons";
import { useNamedProjects, useOwnedBy, useStanding } from "./peer-state";
import { showToast } from "./toast";
import "./board.css";

// What the Delivery block says: whether the ask's words reached the
// orchestrator. An ask typed into the orchestrator's pane went there as it
// was typed, so it has no note.
export function deliveryText(delivery: AskDelivery | null, source: AskDetail["source"]): string {
  if (!delivery) {
    return source === "prompt"
      ? "Typed in the orchestrator's pane"
      : "Not sent: no orchestrator ran when it was asked";
  }
  switch (delivery.state) {
    case "pending":
      return `Waiting to deliver, sent ${formatWhen(delivery.sent_at)}`;
    case "delivered":
      return `Delivered ${formatWhen(delivery.delivered_at)}`;
    case "acked":
      return `Delivered ${formatWhen(delivery.delivered_at)}, seen ${formatWhen(delivery.seen_at)}`;
    case "failed":
      return `Not delivered: ${delivery.failure_reason ?? "the delivery failed"}`;
  }
}

// What became of an ask, once the orchestrator or the owner resolved it.
const RESOLVED: Record<Exclude<AskDisposition, "unresolved">, string> = {
  new: "Filed as a new card",
  followup: "Added to a card",
  chat: "Answered in chat",
  cancelled: "Cancelled",
};

// Cancel takes the ask out of Asked, on its own host: the orchestrator will
// not file it. A peer's ask is cancelled on the peer, over the link.
function CancelAsk({ ask, owner, canAct }: { ask: AskDetail; owner?: string; canAct: boolean }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/asks/${ask.id}/cancel`, {
        idempotency_key,
        ...(owner ? { host: owner } : {}),
      }),
    [ask.id, owner],
  );
  const onRecorded = useCallback(
    (_response: MutationResponse<unknown>) => {
      queryClient.setQueryData<BoardResponse | undefined>(
        queryKeyFor(boardPath()),
        (old) =>
          old && {
            ...old,
            asked: old.asked.filter((row) => !(row.id === ask.id && ownerOf(row) === owner)),
          },
      );
      showToast("ok", "Ask cancelled", owner ? `on ${owner}` : null);
      void navigate({ to: "/", search: (prev) => ({ ...prev, ask: undefined, owner: undefined }) });
    },
    [queryClient, navigate, ask.id, owner],
  );
  const action = useAction({ binding: ask.id, send, onRecorded });
  return (
    <>
      <button
        type="button"
        className="btn"
        disabled={action.pending || !canAct}
        onClick={action.trigger}
        data-testid="ask-cancel"
      >
        {action.pending ? "Cancelling..." : "Cancel"}
      </button>
      <ActionFailure error={action.error} host={owner} />
    </>
  );
}

function Answer({ reply }: { reply: ReplyItem }) {
  return (
    <div className="blk reply-blk" data-testid="ask-answer">
      <div className="blk-h">
        Orchestrator replied
        <span className="hint">
          {formatWhen(reply.replied_at)}
          {reply.read_at ? "" : " - unread"}
        </span>
      </div>
      <div className="reply-a">{reply.body}</div>
    </div>
  );
}

function Body({
  ask,
  owner,
  who,
  listed,
}: {
  ask: AskDetail;
  // The peer that owns the ask, as the URL names it; absent for this host's.
  owner: string | undefined;
  who: Owned;
  // Whether the projects list still has the ask's project, to ask again.
  listed: boolean;
}) {
  const standing = useStanding(who);
  const open = ask.disposition === "unresolved";
  return (
    <>
      <PeerNote standing={standing} />
      <div className="d-meta">
        <span className="lab sm">ask</span>
        <span>
          asked {formatWhen(ask.received_at)} - {formatAgo(ask.received_at)}
        </span>
      </div>
      <div className="blk reply-blk">
        <div className="blk-h">You asked</div>
        <p className="reply-q" data-testid="ask-text">
          {ask.text}
        </p>
      </div>
      <div className="blk reply-blk">
        <div className="blk-h">Delivery</div>
        <p className="ask-state" data-testid="ask-delivery">
          {deliveryText(ask.delivery, ask.source)}
        </p>
      </div>
      {ask.replies.length > 0 ? (
        ask.replies.map((reply) => <Answer key={reply.id} reply={reply} />)
      ) : (
        <div className="blk reply-blk">
          <div className="blk-h">Answer</div>
          <p className="ask-state">No answer yet</p>
        </div>
      )}
      {ask.disposition === "unresolved" ? null : (
        <p className="ask-state" data-testid="ask-resolved">
          {RESOLVED[ask.disposition]} {formatWhen(ask.resolved_at)}
        </p>
      )}
      <div className="acts reply-acts">
        <button
          type="button"
          className="btn"
          disabled={!listed}
          onClick={() =>
            askPrefillStore.set({ project: ownerKey(ask.project_id, who), text: ask.text })
          }
        >
          Ask again
        </button>
        {open ? <CancelAsk ask={ask} owner={owner} canAct={standing.canAct} /> : null}
      </div>
    </>
  );
}

// The panel an ask tile opens beside the board: what was asked, whether it
// reached the orchestrator, every answer, and Ask again and Cancel.
export function AskPanel({ askId, owner }: { askId: string; owner?: string }) {
  const read = useQuery(askQuery(askId, owner));
  // A peer answers the read from its own state.db, so the ask names no host:
  // its host and link come from this board's rows for that peer.
  const asked = useMemo((): Owned & { id: string } => ({ id: askId }), [askId]);
  const who = useOwnedBy(asked, owner);
  const projects = useNamedProjects(useQuery(projectsQuery()).data)?.projects ?? [];
  const ask = read.data;
  const index = ask ? projectIndex(projects, { ...who, project_id: ask.project_id }) : -1;
  const project = projects[index];
  return (
    <div className="card-page ask-panel" data-testid="ask-panel">
      <div className="cp-nav">
        <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
        <span className="muted">{project?.name ?? ask?.project_id ?? "Ask"}</span>
        <ChevronRightIcon />
        <b>ask</b>
        <HostChip entity={who} always />
        <span className="cp-tools">
          <Link
            to="/"
            search={(prev) => ({ ...prev, ask: undefined, owner: undefined })}
            className="ib sm"
            title="Close"
            aria-label="Close"
          >
            <CloseIcon />
          </Link>
        </span>
      </div>
      {ask ? (
        <Body ask={ask} owner={owner} who={who} listed={project !== undefined} />
      ) : read.isError ? (
        <p className="ask-state" role="alert">
          This ask could not be read
          {owner ? `: ${owner} may be out of reach` : ""}. {read.error.message}
        </p>
      ) : null}
    </div>
  );
}
