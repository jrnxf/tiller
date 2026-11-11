import type { RepliesResponse, ReplyItem } from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

const MINUTE = 60_000;

const at = (now: number, minutesAgo: number) => new Date(now - minutesAgo * MINUTE).toISOString();

const cardId = (n: number) => `01JCARD${String(n).padStart(19, "0")}`;

// The replies the orchestrators sent, one per project: an ask that filed a
// card, a card comment, and an ask answered in words alone.
function allReplies(now: number): ReplyItem[] {
  return [
    {
      id: "01JREPLY000000000000000001",
      project: "alpha",
      to: "ask",
      ask_id: "01JASK0000000000000000001",
      message_id: null,
      question: "Can the export job retry on its own when storage times out?",
      asked_at: at(now, 34),
      body: "Yes, but the policy is your call. I filed alpha-41 for it: three options, with exponential backoff recommended. It waits on your decision in the Inbox.",
      replied_at: at(now, 12),
      read_at: null,
      rev: 1,
      card: {
        id: cardId(41),
        display_id: "alpha-41",
        title: "Pick the retry policy for the export job",
        relation: "created",
      },
    },
    {
      id: "01JREPLY000000000000000002",
      project: "beta",
      to: "comment",
      ask_id: null,
      message_id: "01JMSG00000000000000000002",
      question: "Does the cutover doc cover how to roll back?",
      asked_at: at(now, 3 * 60),
      body: "Not yet. I added a rollback step to beta-8's description, so the scout writes it up with the rest.",
      replied_at: at(now, 2 * 60 + 10),
      read_at: null,
      rev: 1,
      card: {
        id: cardId(8),
        display_id: "beta-8",
        title: "Document the cutover steps",
        relation: null,
      },
    },
    {
      id: "01JREPLY000000000000000003",
      project: "gamma",
      to: "ask",
      ask_id: "01JASK0000000000000000003",
      message_id: null,
      question: "How much disk does the cache use now?",
      asked_at: at(now, 26 * 60),
      body: "About 41 GB, most of it build artifacts older than a week. Nothing needs a card: the weekly trim takes it back under 20 GB on Sunday.",
      replied_at: at(now, 25 * 60),
      read_at: null,
      rev: 1,
      card: null,
    },
  ];
}

// What the owner marked read since the stand-in server started.
const readAt = new Map<string, string>();

function hasReplies(scenario: Scenario): boolean {
  return scenario !== "empty" && scenario !== "new-install";
}

// `GET /api/v1/projects/{id}/replies`: the project's unread replies, newest
// first. `undefined` for a project the scenario does not know (a 404).
export function repliesFor(
  scenario: Scenario,
  projectId: string,
  known: boolean,
  now: number,
): RepliesResponse | undefined {
  if (!known) return undefined;
  if (!hasReplies(scenario)) return { replies: [] };
  const replies = allReplies(now).filter(
    (reply) => reply.project === projectId && !readAt.has(reply.id),
  );
  replies.sort((a, b) => b.replied_at.localeCompare(a.replied_at));
  return { replies };
}

// `POST /api/v1/replies/{id}/read`. Marking a read reply again changes
// nothing; an unknown id is `undefined` (a 404).
export function markReplyRead(id: string, now: number): ReplyItem | undefined {
  const reply = allReplies(now).find((candidate) => candidate.id === id);
  if (!reply) return undefined;
  if (!readAt.has(id)) readAt.set(id, new Date(now).toISOString());
  return { ...reply, read_at: readAt.get(id) ?? null, rev: 2 };
}
