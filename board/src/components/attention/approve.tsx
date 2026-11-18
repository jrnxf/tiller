import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";

import type { AttentionItem } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { attentionPath, queryKeyFor } from "../../lib/queries";
import { useStore } from "../../lib/store";
import { PeerNote } from "../board/host-chip";
import { useStanding } from "../board/peer-state";
import { ActionFailure } from "./action-failure";
import { ActionError, sendMutation } from "./api";
import { answeredStore, markAnswered } from "./answered";
import { awaitsDecision, useApprovalPr } from "./approval-facts";
import { shortSha } from "./format";
import { GitIcon, InfoIcon } from "./icons";
import { ReceiptPanel } from "./receipt";
import { useAction } from "./use-action";

type Decision = "accept" | "reject";

interface StaleRefusal {
  shown: string;
  current: string;
}

// A head mismatch is 422 with the new SHA (protocol.md 7.4). Any other 422 is
// some other precondition and keeps its own message.
function staleRefusal(error: ActionError | null, shown: string): StaleRefusal | null {
  if (!error || error.status !== 422) return null;
  const current = error.current?.head_sha;
  if (typeof current === "string" && current !== shown) return { shown, current };
  return null;
}

function staleMessage(refusal: StaleRefusal, prNumber: number | null): string {
  const what = prNumber === null ? "The branch" : `PR #${prNumber}`;
  return `Not approved. ${what} moved from ${shortSha(refusal.shown)} to ${shortSha(refusal.current)} after this page showed it. An approval covers one exact head, so nothing was approved and nothing will merge. Look at the new commits, then approve the new head.`;
}

function useDecide(item: AttentionItem, decision: Decision, onStale: (r: StaleRefusal) => void) {
  const queryClient = useQueryClient();
  const approvalId = item.approval?.id ?? "";
  const headSha = item.approval?.head_sha ?? "";
  const send = useCallback(
    async (idempotency_key: string) => {
      try {
        return await sendMutation("POST", `/api/v1/approvals/${approvalId}/decide`, {
          decision,
          head_sha: headSha,
          rev: item.rev,
          ...hostField(item),
          idempotency_key,
        });
      } catch (caught) {
        const stale = staleRefusal(caught instanceof ActionError ? caught : null, headSha);
        if (stale) {
          onStale(stale);
          // Fetch the item again so the new head is what the board shows next.
          void queryClient.invalidateQueries({ queryKey: queryKeyFor(attentionPath) });
        }
        throw caught;
      }
    },
    [approvalId, decision, headSha, item, onStale, queryClient],
  );
  const onRecorded = useCallback(
    () => markAnswered(item, decision === "accept" ? `Approved ${shortSha(headSha)}` : "Rejected"),
    [decision, headSha, item],
  );
  return useAction({
    // A new head is a new decision, so it gets a new idempotency key.
    binding: `${approvalId}:${headSha}:${decision}`,
    receiptKey: `attention:${item.id}`,
    send,
    onRecorded,
  });
}

// An approval while the checks fail can only end unused: the daemon marks it
// stale at once and merges nothing. So Approve is off until they pass. Reject
// stays on.
const CHECKS_FAILED = "The checks failed, so an approval would merge nothing.";

// What stands in place of the buttons on a merge item that asks for no
// decision: what happened to the approval, what comes next, and how the note
// leaves the list. The item's question says why (the daemon's words).
function ApprovalNotice({ item }: { item: AttentionItem }) {
  const approval = item.approval;
  if (!approval) return null;
  const sha = shortSha(approval.head_sha);
  const text =
    approval.state === "stale"
      ? {
          head: "Nothing merged",
          body: `The approval of ${sha} ended before the merge, so there is nothing to approve here. The orchestrator asks for a new approval when the pull request is ready. That request shows as its own item.`,
        }
      : approval.state === "accepted"
        ? {
            head: "Your approval stands",
            body: `The approval of ${sha} is saved. The daemon lands the pull request by itself once this is fixed.`,
          }
        : approval.state === "executing"
          ? { head: "The merge is under way", body: `The daemon merges ${sha} now.` }
          : {
              head: `This approval is ${approval.state}`,
              body: "There is nothing more to decide here.",
            };
  return (
    <div className="appr apn" role="note" data-testid="approval-notice">
      <div className="a-h">
        <InfoIcon />
        {text.head}
      </div>
      <p>{text.body}</p>
      <p className="apn-sub">
        This note leaves the list when the approval ends, a new request comes or the card closes.
        {ownerOf(item) ? null : (
          <>
            {" "}
            To remove it now:{" "}
            <code>tiller attention withdraw {item.id.slice(-7)} --reason seen</code>
          </>
        )}
      </p>
    </div>
  );
}

// Approve or reject, bound to the head SHA the board shows (state.md, Approval).
export function ApproveBlock({ item }: { item: AttentionItem }) {
  return awaitsDecision(item) || !item.approval ? (
    <ApproveBlockFor item={item} />
  ) : (
    <ApprovalNotice item={item} />
  );
}

function ApproveBlockFor({ item }: { item: AttentionItem }) {
  const [refusal, setRefusal] = useState<StaleRefusal | null>(null);
  const answered = useStore(answeredStore)[item.id]?.answer;
  const standing = useStanding(item);
  const off = standing.canAct ? null : standing.reason;
  const accept = useDecide(item, "accept", setRefusal);
  const reject = useDecide(item, "reject", setRefusal);
  const checksFailed = useApprovalPr(item).pr?.checks === "failing";
  const approval = item.approval;
  if (!approval) return null;

  const headSha = approval.head_sha;
  const recorded = accept.recorded;
  const pending = accept.pending || reject.pending;
  // The refusal stays up after the new head arrives: it says what happened and
  // asks for exactly what the enabled button now does. If the head moved again
  // since, the newest one is named.
  const shown =
    refusal && !recorded
      ? { shown: refusal.shown, current: headSha === refusal.shown ? refusal.current : headSha }
      : null;
  const error = accept.error ?? reject.error;
  const otherError = error && !staleRefusal(error, refusal?.shown ?? headSha) ? error : null;
  const rejected = answered === "Rejected";

  return (
    <>
      <div className={`appr${shown ? " stale" : ""}`} data-testid="approval">
        <div className="a-h">
          {shown ? <InfoIcon /> : <GitIcon />}
          {shown ? "The head moved" : "Approval bound to this head"}
        </div>
        <div className="a-row">
          <div className="bind">
            <span>
              {approval.pr_number === null ? "Branch head" : `PR #${approval.pr_number} head`}
            </span>
            {shown ? (
              <>
                <span className="sha old">{shortSha(shown.shown)}</span>
                <span className="sha new">{shortSha(shown.current)}</span>
              </>
            ) : (
              <span className="sha" title={headSha}>
                {shortSha(headSha)}
              </span>
            )}
          </div>
          {recorded ? null : (
            <div className="acts">
              <button
                type="button"
                className="btn lg"
                disabled={pending || off !== null}
                title={off ?? undefined}
                onClick={reject.trigger}
              >
                {reject.pending ? "Rejecting..." : "Reject"}
              </button>
              <button
                type="button"
                className="btn primary lg"
                // Until the new head arrives there is nothing current to approve.
                disabled={pending || headSha === shown?.shown || off !== null || checksFailed}
                title={off ?? (checksFailed ? CHECKS_FAILED : undefined)}
                onClick={accept.trigger}
              >
                {accept.pending ? "Approving..." : `Approve ${shortSha(headSha)}`}
              </button>
            </div>
          )}
        </div>
        <PeerNote standing={standing} />
        {shown ? (
          <p className="refusal" role="alert" data-testid="stale-refusal">
            {staleMessage(shown, approval.pr_number)}
          </p>
        ) : null}
        <ActionFailure error={otherError} host={ownerOf(item)} />
      </div>
      <ReceiptPanel
        receiptKey={accept.receiptKey}
        label={answered ?? `Approved ${shortSha(headSha)}`}
        text={
          rejected
            ? {
                recorded: "Saved: not approved",
                delivered: "Nothing to merge",
                done: "The orchestrator hears it on its next turn",
              }
            : {
                recorded: `Saved an approval bound to ${shortSha(headSha)}`,
                delivered: "The daemon re-read the head and started the merge",
                done: "Merged",
              }
        }
      />
    </>
  );
}

// Reject then Approve, right-aligned, for a page that already shows the
// approval's details (the card page). Same binding as the inbox: the head SHA
// shown, once. Reject needs no reason.
export function ApproveActions({ item }: { item: AttentionItem | undefined }) {
  return item ? <ApproveActionsFor item={item} /> : null;
}

function ApproveActionsFor({ item }: { item: AttentionItem }) {
  return awaitsDecision(item) ? <ApproveButtons item={item} /> : <ApprovalNotice item={item} />;
}

function ApproveButtons({ item }: { item: AttentionItem }) {
  const [refusal, setRefusal] = useState<StaleRefusal | null>(null);
  const standing = useStanding(item);
  const off = standing.canAct ? null : standing.reason;
  const accept = useDecide(item, "accept", setRefusal);
  const reject = useDecide(item, "reject", setRefusal);
  const checksFailed = useApprovalPr(item).pr?.checks === "failing";
  const approval = item.approval;
  if (!approval) return null;
  const headSha = approval.head_sha;
  if (accept.recorded) {
    return (
      <p className="bind" role="status" data-testid="approve-recorded">
        {`Saved an approval bound to ${shortSha(headSha)}`}
      </p>
    );
  }
  const stale = refusal
    ? { shown: refusal.shown, current: headSha === refusal.shown ? refusal.current : headSha }
    : null;
  const error = accept.error ?? reject.error;
  const otherError = error && !staleRefusal(error, refusal?.shown ?? headSha) ? error : null;
  const pending = accept.pending || reject.pending;
  return (
    <div className="prc-act">
      {stale ? (
        <p className="refusal" role="alert" data-testid="stale-refusal">
          {staleMessage(stale, approval.pr_number)}
        </p>
      ) : null}
      <div className="acts right">
        <button
          type="button"
          className="btn"
          disabled={pending || off !== null}
          title={off ?? undefined}
          onClick={reject.trigger}
        >
          {reject.pending ? "Rejecting..." : "Reject"}
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={pending || headSha === stale?.shown || off !== null || checksFailed}
          title={off ?? (checksFailed ? CHECKS_FAILED : undefined)}
          onClick={accept.trigger}
        >
          {accept.pending ? "Approving..." : `Approve ${shortSha(headSha)}`}
        </button>
      </div>
      <p className="meta">
        {checksFailed
          ? "Approve is off until the checks pass."
          : // The button names the commit it binds.
            "A new push asks again."}
      </p>
      <ActionFailure error={otherError} host={ownerOf(item)} />
    </div>
  );
}
