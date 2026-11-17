import type {
  AgentRow,
  AttentionItem,
  BoardRow,
  Project,
  ReplyItem,
  ThreadAsk,
  ThreadContext,
  ThreadDecision,
  ThreadItem,
  ThreadStep,
} from "../../lib/api-types";
import type { AskPiece } from "../../lib/ask-plan";
import { ownerKey, ownerOf, projectKey } from "../../lib/peers";
import { agentActivity } from "../board/activity";

// What the chat shows, worked out from the reads with no React: the sidebar
// groups, the receipt under a sent note, the queue and the tool status.

export type ChatGroup = "needs" | "working" | "idle";

export const GROUP_LABELS: Record<ChatGroup, string> = {
  needs: "Needs you",
  working: "Working",
  idle: "Idle",
};

// One project's row in the chat list.
export interface ChatRow {
  project: Project;
  // Its place in the projects list, which picks its color.
  index: number;
  group: ChatGroup;
  orchestrator: AgentRow | null;
  // Open decisions on its cards.
  waiting: number;
  unread: number;
  // The newest unread reply, else the orchestrator's last status line.
  snippet: string;
  at: string | null;
}

// The orchestrator of a project, matched by host as well as by id: `fleet`
// exists on every host.
function orchestratorOf(project: Project, orchestrators: AgentRow[]): AgentRow | null {
  return (
    orchestrators.find(
      (agent) => agent.project_id === project.id && ownerOf(agent) === ownerOf(project),
    ) ?? null
  );
}

// The open questions of each project, keyed as `projectKey` keys it. An item
// names its card; the board says which project the card is in.
export function waitingByProject(
  attention: AttentionItem[],
  rows: BoardRow[],
): Map<string, number> {
  const projectOfCard = new Map(rows.map((row) => [row.id, ownerKey(row.project_id, row)]));
  const counts = new Map<string, number>();
  for (const item of attention) {
    if (item.answered_at !== null || item.withdrawn_at !== null) continue;
    const key = projectOfCard.get(item.card_id);
    if (key) counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return counts;
}

// Needs you: a question waits on the owner, or the orchestrator waits on a
// prompt. Working: the orchestrator is working now. Idle: everything else,
// with no orchestrator running too.
export function chatRows(
  projects: Project[],
  orchestrators: AgentRow[],
  waiting: Map<string, number>,
  replies: ReplyItem[],
  now?: number,
): ChatRow[] {
  return projects.map((project, index) => {
    const key = projectKey(project);
    const orchestrator = orchestratorOf(project, orchestrators);
    const view = orchestrator ? agentActivity(orchestrator, now) : null;
    const open = waiting.get(key) ?? 0;
    const group: ChatGroup =
      open > 0 || view?.dot === "waiting" ? "needs" : view?.dot === "live" ? "working" : "idle";
    const own = replies.filter((reply) => ownerKey(reply.project, reply) === key && !reply.read_at);
    const newest = own.reduce<ReplyItem | null>(
      (best, reply) => (best === null || reply.replied_at > best.replied_at ? reply : best),
      null,
    );
    const status = orchestrator?.last_status_line ?? null;
    return {
      project,
      index,
      group,
      orchestrator,
      waiting: open,
      unread: own.length,
      snippet:
        newest?.body ?? status ?? (orchestrator ? "No status yet" : "No orchestrator running"),
      at:
        newest?.replied_at ?? orchestrator?.last_status_at ?? orchestrator?.last_output_at ?? null,
    };
  });
}

// The rows of each group, in list order, without the empty groups. A search
// keeps the projects whose name holds the typed letters.
export function groupRows(rows: ChatRow[], query: string): { group: ChatGroup; rows: ChatRow[] }[] {
  const needle = query.trim().toLowerCase();
  const shown = needle
    ? rows.filter((row) => row.project.name.toLowerCase().includes(needle))
    : rows;
  return (["needs", "working", "idle"] as const).flatMap((group) => {
    const members = shown.filter((row) => row.group === group);
    return members.length > 0 ? [{ group, rows: members }] : [];
  });
}

// The line under a sent note: how far it got.
export interface Receipt {
  tone: "sending" | "delivered" | "read" | "failed";
  text: string;
}

export function receiptOf(
  ask: ThreadAsk,
  clock: (iso: string | null) => string | null,
): Receipt | null {
  const delivery = ask.delivery;
  // Typed in the orchestrator's pane: no note went through the outbox.
  if (!delivery) return null;
  switch (delivery.state) {
    case "pending":
      return { tone: "sending", text: "Sending" };
    case "delivered": {
      const at = clock(delivery.delivered_at);
      return { tone: "delivered", text: at ? `Delivered ${at}` : "Delivered" };
    }
    case "acked": {
      const at = clock(delivery.seen_at);
      return { tone: "read", text: at ? `Read by orchestrator ${at}` : "Read by orchestrator" };
    }
    case "failed":
      return {
        tone: "failed",
        text: delivery.failure_reason
          ? `Not delivered: ${delivery.failure_reason}`
          : "Not delivered",
      };
  }
}

// The replies the thread shows that the owner has not read. Opening the
// chat marks them read in one call.
export function unreadReplies(items: ThreadItem[]): string[] {
  return items.flatMap((item) =>
    item.type === "reply" && !item.reply.read_at ? [item.reply.id] : [],
  );
}

// The project's cards as the orchestrator's queue: what runs now, what waits,
// and what finished. A running card names the worker on it.
export interface QueueEntry {
  row: BoardRow;
  worker: string | null;
}

export interface Queue {
  running: QueueEntry[];
  next: QueueEntry[];
  done: QueueEntry[];
}

// The queue keeps the newest few done cards: the rest are history.
const DONE_SHOWN = 3;

const byPriority = (a: BoardRow, b: BoardRow) => a.priority - b.priority;

export function queueOf(project: Project, rows: BoardRow[], orchestrator: AgentRow | null): Queue {
  const key = projectKey(project);
  const own = rows.filter((row) => ownerKey(row.project_id, row) === key);
  const workers = orchestrator?.workers ?? [];
  const entry = (row: BoardRow): QueueEntry => ({
    row,
    worker: workers.find((worker) => worker.card_id === row.id)?.name ?? null,
  });
  const running = own.filter((row) => row.column === "working" || row.column === "landing");
  const next = own.filter((row) => row.column === "queued");
  const done = own.filter((row) => row.column === "done");
  running.sort(byPriority);
  next.sort(byPriority);
  done.sort((a, b) => (b.closed_at ?? "").localeCompare(a.closed_at ?? ""));
  return {
    running: running.map(entry),
    next: next.map(entry),
    done: done.slice(0, DONE_SHOWN).map(entry),
  };
}

// "1 running · 2 next", the chip's words; null with nothing in the queue.
export function queueChip(queue: Queue): string | null {
  const parts = [
    queue.running.length > 0 ? `${queue.running.length} running` : null,
    queue.next.length > 0 ? `${queue.next.length} next` : null,
  ].filter((part) => part !== null);
  return parts.length > 0 ? parts.join(" · ") : null;
}

// "1 running, 2 up next, 1 done", the popover's hint.
export function queueHint(queue: Queue): string {
  return `${queue.running.length} running, ${queue.next.length} up next, ${queue.done.length} done`;
}

// A daemon step label is a lowercase phrase ("created the card"); the chat
// shows it as a sentence.
export function stepTitle(label: string): string {
  return label.charAt(0).toUpperCase() + label.slice(1);
}

// A step failed when its event says so or its outcome does.
export function stepFailed(step: ThreadStep): boolean {
  return step.kind.includes("fail") || step.payload.outcome === "failed";
}

// "Worked for 12 seconds", "Worked for 3 minutes".
export function workedFor(seconds: number): string {
  if (seconds < 60) return `Worked for ${seconds} second${seconds === 1 ? "" : "s"}`;
  const minutes = Math.round(seconds / 60);
  return `Worked for ${minutes} minute${minutes === 1 ? "" : "s"}`;
}

// "62k", the token count the context ring shows.
export function tokens(count: number): string {
  if (count < 1000) return String(count);
  return `${Math.round(count / 1000)}k`;
}

// How much of the window is in use, from 0 to 1; null with no window known.
export function contextShare(context: ThreadContext): number | null {
  if (!context.window || context.window <= 0) return null;
  return Math.min(1, Math.max(0, context.used / context.window));
}

// The follow-ups under the latest reply: a fixed set by the project's state.
export function suggestionsFor(group: ChatGroup): string[] {
  switch (group) {
    case "needs":
      return ["What do you need from me?", "What happens if I wait?"];
    case "working":
      return ["What are you working on now?", "When will it land?"];
    case "idle":
      return ["What should we do next?", "Summarize what landed today"];
  }
}

// What a closed decision says: the owner's choice and when, or why it closed
// without one.
export function decisionText(
  decision: ThreadDecision,
  clock: (iso: string | null) => string | null,
): string {
  const at = clock(decision.decided_at ?? null);
  const when = at ? ` at ${at}` : "";
  if (decision.state === "withdrawn") return "The orchestrator withdrew it";
  if (decision.choice) {
    const choice =
      decision.source === "approval"
        ? decision.choice === "accepted"
          ? "Approve"
          : decision.choice === "rejected"
            ? "Reject"
            : decision.choice
        : decision.choice;
    return `You chose ${choice}${when}`;
  }
  return `Closed${when}`;
}

// The words the orchestrator reads: the text with each card chip as its id,
// so "#alpha-41 is next" reads "alpha-41 is next".
export function chatText(pieces: AskPiece[]): string {
  return pieces
    .map((piece) => (piece.kind === "mention" ? piece.name : piece.text))
    .join("")
    .replace(/[ \t]{2,}/g, " ")
    .trim();
}

// The words a reply is quoted with in the box: its first lines, each marked.
const QUOTE_CHARS = 280;

export function quoteOf(body: string): string {
  const cut = body.length > QUOTE_CHARS ? `${body.slice(0, QUOTE_CHARS).trimEnd()}…` : body;
  return `${cut
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n")}\n\n`;
}
