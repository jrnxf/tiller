import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";

import type { AttentionItem } from "../../lib/api-types";
import { attentionPath, queryKeyFor } from "../../lib/queries";
import { useStore } from "../../lib/store";
import { ActionError, sendMutation } from "./api";
import { answeredStore, markAnswered } from "./answered";
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
    [approvalId, decision, headSha, item.rev, onStale, queryClient],
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

// Approve or reject, bound to the head SHA the board shows (state.md, Approval).
export function ApproveBlock({ item }: { item: AttentionItem }) {
  const [refusal, setRefusal] = useState<StaleRefusal | null>(null);
  const answered = useStore(answeredStore)[item.id]?.answer;
  const accept = useDecide(item, "accept", setRefusal);
  const reject = useDecide(item, "reject", setRefusal);
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
        {shown ? (
          <p className="refusal" role="alert" data-testid="stale-refusal">
            {staleMessage(shown, approval.pr_number)}
          </p>
        ) : null}
        {recorded ? null : (
          <div className="acts">
            <button
              type="button"
              className="btn primary lg"
              // Until the new head arrives there is nothing current to approve.
              disabled={pending || headSha === shown?.shown}
              onClick={accept.trigger}
            >
              {accept.pending ? "Approving..." : `Approve ${shortSha(headSha)}`}
            </button>
            <button type="button" className="btn lg" disabled={pending} onClick={reject.trigger}>
              {reject.pending ? "Rejecting..." : "Reject"}
            </button>
          </div>
        )}
        {otherError ? (
          <p className="refusal" role="alert">
            {otherError.message}
          </p>
        ) : null}
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
