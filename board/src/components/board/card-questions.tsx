import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import type { AttentionItem } from "../../lib/api-types";
import type { Owned } from "../../lib/peers";
import { agentsQuery } from "../../lib/queries";
import { useStore } from "../../lib/store";
import { AnswerForm } from "../attention/answer";
import { ApprovalFacts } from "../attention/approval-facts";
import { ApproveActions } from "../attention/approve";
import { answeredStore } from "../attention/answered";
import { KIND_LABELS, recommender } from "../attention/format";
import { NeedsIcon, OkIcon } from "../attention/icons";
import { Ago } from "./ago";

// What the card asks of the owner now, at the top of the card and answered in
// place with the Inbox's own answer control. A card that says "Decision"
// shows the decision, and one that says "Merge approval" shows the pull
// request with Approve and Reject.
export function CardQuestions({
  cardId,
  attention,
  owner,
}: {
  cardId: string;
  attention: AttentionItem[];
  // The card's row: a peer's card names its host and link, and a forwarded
  // card read sends items that name neither.
  owner: Owned;
}) {
  const answeredHere = useStore(answeredStore);
  const agents = useQuery(agentsQuery()).data?.orchestrators ?? [];
  // An item answered from this tab stays, with its receipt, until a reload,
  // as it does in the Inbox. The card read drops it once it is answered, so
  // it comes from what this tab sent.
  const open = attention.filter(
    (item) => item.still_current && item.withdrawn_at === null && item.answered_at === null,
  );
  const listed = new Set(open.map((item) => item.id));
  const sent = Object.values(answeredHere)
    .map((entry) => entry.item)
    .filter((item) => item.card_id === cardId && !listed.has(item.id));
  const items = [...sent, ...open].map((item) =>
    owner.host && !item.host
      ? { ...item, host: owner.host, as_of: owner.as_of ?? null, link: owner.link ?? null }
      : item,
  );
  if (items.length === 0) return null;
  // Keys 1-9 answer the first question that still waits, never two at once.
  const keyed = open.find((item) => !answeredHere[item.id]);
  return (
    <section className="cq" aria-label="Needs you" data-testid="card-questions">
      {items.map((item) => {
        const by = recommender(item.opened_by_session_id, agents);
        const done = answeredHere[item.id] !== undefined;
        return (
          <div className={done ? "cq-item done" : "cq-item"} key={item.id}>
            <div className="cq-meta">
              {done ? <OkIcon /> : <NeedsIcon />}
              <b>{done ? "Answered" : "Needs you"}</b>
              <span className="lab sm">{KIND_LABELS[item.kind] ?? "Decision"}</span>
              <Ago at={item.opened_at} prefix="asked" />
              <Link
                to="/inbox"
                search={({ host }) => ({ host, item: item.id })}
                className="cq-open"
              >
                Open in the Inbox
              </Link>
            </div>
            <h5 className="cq-q">{item.question}</h5>
            {item.consequence ? <p className="cq-ctx">{item.consequence}</p> : null}
            {item.recommendation ? (
              <div className="rec">
                <span className="av lg">{by.initial}</span>
                <div className="tx">
                  <span className="who">{by.text}</span>
                  {item.recommendation}
                </div>
              </div>
            ) : null}
            <div className="cq-answer">
              {item.approval ? (
                <>
                  <ApprovalFacts item={item} />
                  <ApproveActions item={item} />
                </>
              ) : (
                <AnswerForm item={item} keys={item.id === keyed?.id} />
              )}
            </div>
          </div>
        );
      })}
    </section>
  );
}
