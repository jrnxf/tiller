import type { Editor, JSONContent } from "@tiptap/core";
import { lazy, Suspense, useEffect, useId, useMemo, useRef, useState } from "react";
import type { RefObject } from "react";

import type { AgentRow, BoardRow, Project } from "../../lib/api-types";
import type { AskPiece } from "../../lib/ask-plan";
import { mentionedKeys } from "../../lib/ask-plan";
import { ownerOf } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { SendIcon, StopIcon } from "../board/icons";
import { TerminalInput } from "../board/terminal-stream";
import { showToast } from "../board/toast";
import { LayersIcon } from "../shell/icons";
import type { MentionOption } from "../shell/ask-mentions";
import "../shell/ask-dialog.css";
import { HashIcon } from "./icons";
import { chatText, modelName, queueChip, queueHint } from "./model";
import type { Queue, QueueEntry } from "./model";

// The editor is the heaviest code on the page: it loads on its own.
const AskEditor = lazy(async () => ({ default: (await import("../shell/ask-editor")).AskEditor }));

const CARDS = { char: "#", label: "Cards", empty: "No card matches" };

// Plain text as editor content: one paragraph per line.
function docOf(text: string): JSONContent {
  return {
    type: "doc",
    content: text.split("\n").map((line) => ({
      type: "paragraph",
      content: line ? [{ type: "text", text: line }] : [],
    })),
  };
}

// Puts these words in the box, replacing what was there, and the caret after them.
export function fillEditor(editor: Editor | null, text: string) {
  if (!editor) return;
  editor.commands.setContent(docOf(text), { emitUpdate: true });
  editor.commands.focus("end");
}

function QueueRows({
  title,
  entries,
  state,
}: {
  title: string;
  entries: QueueEntry[];
  state: string;
}) {
  if (entries.length === 0) return null;
  return (
    <>
      <li className="chat-qsec">{title}</li>
      {entries.map(({ row, worker }) => (
        <li key={row.id} className="chat-qi" data-s={state}>
          <span className="c" aria-hidden="true" />
          <span className="t">
            <span className="id">{row.display_id}</span>
            {row.title}
          </span>
          {worker ? <span className="who">{worker}</span> : <span />}
        </li>
      ))}
    </>
  );
}

// The orchestrator's cards over the box: what runs, what waits, what finished.
function QueuePopover({ queue, id, onClose }: { queue: Queue; id: string; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && ref.current?.contains(target)) return;
      if (target instanceof Element && target.closest(".chat-qbtn")) return;
      onClose();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);
  const empty = queue.running.length + queue.next.length + queue.done.length === 0;
  return (
    <div
      className="chat-qpop"
      id={id}
      ref={ref}
      role="dialog"
      aria-label="Queue"
      data-testid="chat-queue"
    >
      <div className="chat-qhead">
        <span>Queue</span>
        <span className="chat-qhint">{queueHint(queue)}</span>
      </div>
      {empty ? (
        <p className="chat-qempty">No cards in this project yet.</p>
      ) : (
        <ul className="chat-qlist">
          <QueueRows title="Running" entries={queue.running} state="run" />
          <QueueRows title="Up next" entries={queue.next} state="next" />
          <QueueRows title="Done" entries={queue.done} state="done" />
        </ul>
      )}
    </div>
  );
}

// The box at the foot of the chat: the message with `#` card chips, the
// queue, the orchestrator's model, and Send, or Stop while it works.
export function ChatPrompt({
  project,
  index,
  orchestrator,
  working,
  rows,
  queue,
  editorRef,
  pending,
  send,
}: {
  project: Project;
  index: number;
  orchestrator: AgentRow | null;
  working: boolean;
  // The project's cards, which `#` offers.
  rows: BoardRow[];
  queue: Queue;
  editorRef: RefObject<Editor | null>;
  // The message on its way, which holds the next one until it lands.
  pending: string | null;
  send: (text: string) => Promise<boolean>;
}) {
  const labelId = useId();
  const queueId = useId();
  const [pieces, setPieces] = useState<AskPiece[]>([]);
  const [queueOpen, setQueueOpen] = useState(false);
  const text = chatText(pieces);
  const sending = pending !== null;
  const color = projectColor(index);
  const options = useMemo<MentionOption[]>(
    () =>
      rows.map((row) => ({
        key: row.id,
        name: row.display_id,
        detail: row.title,
        color,
        host: null,
        off: null,
      })),
    [rows, color],
  );

  // The box empties at once. A send that fails puts the words back, unless
  // the owner has started on the next message.
  const submit = () => {
    if (sending || text === "") return;
    const words = text;
    editorRef.current?.commands.clearContent(true);
    void send(words).then((sent) => {
      const editor = editorRef.current;
      if (!sent && editor?.isEmpty) fillEditor(editor, words);
    });
  };

  // Esc in the orchestrator's pane, as a person at its terminal would press
  // it: the board has no interrupt for an orchestrator, which has no run.
  const stop = () => {
    if (!orchestrator) return;
    const input = new TerminalInput(
      orchestrator.session_id,
      (title, detail) => showToast("bad", title, detail),
      ownerOf(orchestrator),
    );
    input.keys(["esc"]);
    showToast("ok", "Sent Esc to the orchestrator");
  };

  const chip = queueChip(queue);
  const showStop = working && text === "" && !sending;
  return (
    <>
      {queueOpen ? (
        <QueuePopover queue={queue} id={queueId} onClose={() => setQueueOpen(false)} />
      ) : null}
      <form
        className="chat-prompt"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <span id={labelId} className="sr-only">
          Message {project.name}
        </span>
        <Suspense fallback={<div className="ask-ed" />}>
          <AskEditor
            options={options}
            used={mentionedKeys(pieces)}
            locked={false}
            placeholder={`Message ${project.name}. # links a card.`}
            labelledBy={labelId}
            editorRef={editorRef}
            onChange={setPieces}
            onSubmit={submit}
            onListOpen={() => {}}
            trigger={CARDS}
          />
        </Suspense>
        <div className="chat-ptools">
          <div className="l">
            <button
              type="button"
              className="chat-tbtn sq"
              aria-label="Link a card"
              title="Link a card"
              onClick={() => editorRef.current?.chain().focus().insertContent("#").run()}
            >
              <HashIcon />
            </button>
            <button
              type="button"
              className="chat-tbtn chat-qbtn"
              aria-expanded={queueOpen}
              aria-controls={queueOpen ? queueId : undefined}
              onClick={() => setQueueOpen((open) => !open)}
              data-testid="chat-queue-chip"
            >
              {queue.running.length > 0 ? (
                <span className="chat-qdot" aria-hidden="true" />
              ) : (
                <LayersIcon />
              )}
              <span>{chip ?? "Queue"}</span>
            </button>
          </div>
          <div className="r">
            <span
              className="chat-model"
              title={orchestrator ? "The orchestrator's model" : "Sending starts the orchestrator"}
            >
              {orchestrator?.model
                ? modelName(orchestrator.model)
                : orchestrator
                  ? null
                  : "Not running"}
            </span>
            {showStop ? (
              <button
                type="button"
                className="chat-submit stop"
                aria-label="Stop"
                title="Send Esc to the orchestrator"
                onClick={stop}
              >
                <StopIcon />
              </button>
            ) : (
              <button
                type="submit"
                className="chat-submit"
                aria-label="Send"
                title="Send (Enter)"
                disabled={sending || text === ""}
              >
                <SendIcon />
              </button>
            )}
          </div>
        </div>
      </form>
    </>
  );
}
