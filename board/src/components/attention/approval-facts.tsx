import { useQuery } from "@tanstack/react-query";

import type { AttentionItem, CardPr } from "../../lib/api-types";
import { ownerOf } from "../../lib/peers";
import { cardQuery } from "../../lib/queries";
import { CheckIcon } from "../shell/icons";
import { GitIcon } from "./icons";

// What the checks of a pull request say, in the words the owner decides on.
function checksText(pr: CardPr): { tone: "ok" | "bad" | "yel"; text: string } {
  if (pr.checks === "passing") return { tone: "ok", text: pr.checks_summary ?? "Checks passed" };
  if (pr.checks === "failing") return { tone: "bad", text: pr.checks_summary ?? "Checks failed" };
  return { tone: "yel", text: pr.checks_summary ?? "Checks still run" };
}

// What an approval lands: the pull request's title, a link to read it, its
// checks and where it merges. The approval item itself carries only a commit
// hash, so this reads the card. It sits right above the Approve button, in
// the Inbox and at the top of the card.
// The pull request an approval item binds, from the card's read. `pr` is
// undefined until the card arrives, and for a land with no pull request.
export function useApprovalPr(item: AttentionItem): { pr: CardPr | undefined; failed: boolean } {
  const approval = item.approval;
  const read = useQuery({
    ...cardQuery(item.card_id, ownerOf(item)),
    enabled: approval !== null,
  });
  const detail = read.data;
  const bound = detail?.approvals.find((entry) => entry.id === approval?.id);
  const pr = approval
    ? (detail?.prs.find((entry) => entry.id === bound?.pr_id) ??
      detail?.prs.find((entry) => entry.number === approval.pr_number))
    : undefined;
  return { pr, failed: read.isError };
}

// A merge item asks for a decision only while its approval is `requested`.
// The daemon also opens merge items about an approval that is already decided
// or ended (failed checks after an approval, a pull request body it will not
// merge). Those say what happened; a decision on them is always refused.
export function awaitsDecision(item: AttentionItem): boolean {
  return item.approval?.state === "requested";
}

export function ApprovalFacts({ item }: { item: AttentionItem }) {
  const approval = item.approval;
  const { pr, failed } = useApprovalPr(item);
  if (!approval) return null;
  if (!pr) {
    // A land with no pull request has nothing more to show than its head.
    if (approval.pr_number === null) return null;
    return (
      <p className="apf-note" data-testid="approval-facts">
        {failed
          ? `The details of PR #${approval.pr_number} did not load. Open the card to read them.`
          : `Reading PR #${approval.pr_number}...`}
      </p>
    );
  }
  const checks = checksText(pr);
  return (
    <div className="apf" data-testid="approval-facts">
      <div className="apf-h">
        <GitIcon />
        <span className="apf-t">{pr.title}</span>
        <a className="apf-link" href={pr.url} target="_blank" rel="noreferrer noopener">
          Open {pr.repo}#{pr.number}
        </a>
      </div>
      <dl>
        <dt>Checks</dt>
        <dd>
          <span className={`lab sm ${checks.tone}`}>
            {pr.checks === "passing" ? <CheckIcon /> : null}
            {checks.text}
          </span>
          {pr.checks === "passing" || !awaitsDecision(item) ? null : (
            <span className="apf-warn">
              {pr.checks === "failing"
                ? "Approve is off: the daemon lands only a head whose checks pass."
                : "An approval now merges once the checks pass."}
            </span>
          )}
        </dd>
        <dt>Merges into</dt>
        <dd className="mono">{pr.base}</dd>
        {pr.diff_summary ? (
          <>
            <dt>Diff</dt>
            <dd>{pr.diff_summary}</dd>
          </>
        ) : null}
      </dl>
    </div>
  );
}
