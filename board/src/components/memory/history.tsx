import { useRef, useState } from "react";
import type { ReactNode } from "react";

import type { MemoryChange } from "../../lib/api-types";
import type { ActionError } from "../attention/api";
import { Modal } from "../ui/modal";
import { Inline, oneLine, when } from "./parts";

// A document's change history (memory.md section 7, Retire and roll back):
// what was kept, added, edited, removed or discarded in the last 30 days, at
// the foot of the document, each with Undo... where it is read.

const LABEL: Record<MemoryChange["kind"], string> = {
  kept: "Kept",
  added: "Added",
  edit: "Edited",
  remove: "Removed",
  discard: "Discarded",
};

// Kept and added lines read green, a discard reads grey, and the rest plain.
const TONE: Record<MemoryChange["kind"], string> = {
  kept: " ok",
  added: " ok",
  edit: "",
  remove: "",
  discard: " mute",
};

function ChangeText({ change }: { change: MemoryChange }) {
  const text = change.text ?? "";
  if (change.undone_at !== null) return <del className="del">{text}</del>;
  if (change.kind === "remove") return <del className="del">{text}</del>;
  if (change.kind === "discard") return <span className="h-mute">{text}</span>;
  // An edit shows the line as it reads now; its undo says what it goes back to.
  if (change.kind === "edit") return <>{text}</>;
  return <ins className="ins">{text}</ins>;
}

// "Testing · the curator from alpha-12 · kept by you, Tue 10 Nov, 15:30".
function meta(change: MemoryChange): string {
  const written = change.card ? `${change.by} from ${change.card}` : change.by;
  const at = when(change.at);
  const done =
    change.undone_at !== null
      ? `undone by you, ${when(change.undone_at)}`
      : change.kind === "kept"
        ? `kept by you, ${at}`
        : change.kind === "discard"
          ? `discarded by you, ${at}`
          : null;
  if (done === null) return [change.section, `${written}, ${at}`].join(" · ");
  return [change.section, written, done].join(" · ");
}

// What an undo does to the document, said before it is sent.
function undoes(change: MemoryChange, doc: string): string {
  const text = `"${oneLine(change.text)}"`;
  const record = "A new commit records the undo, and the history shows both.";
  switch (change.kind) {
    case "kept":
    case "added":
      return `The line ${text} leaves ${doc}'s memory. ${record}`;
    case "edit":
      return change.old_text
        ? `The line goes back to "${oneLine(change.old_text)}". ${record}`
        : `The edit to ${text} is reversed. ${record}`;
    case "remove":
      return `The line ${text} comes back to ${doc}'s memory. ${record}`;
    case "discard":
      return `The suggestion ${text} comes back for you to review. Nothing in the file changes until you keep it.`;
  }
}

function UndoDialog({
  change,
  doc,
  onKeep,
  onUndo,
}: {
  change: MemoryChange;
  doc: string;
  onKeep: () => void;
  onUndo: () => void;
}) {
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <Modal
      className="dialog"
      labelledBy="undo-dialog-title"
      describedBy="undo-dialog-body"
      onClose={onKeep}
      initialFocus={keep}
    >
      <h2 id="undo-dialog-title">
        {change.kind === "discard" ? "Undo this discard?" : "Undo this change?"}
      </h2>
      <p id="undo-dialog-body">{undoes(change, doc)}</p>
      <div className="dialog-actions">
        <button ref={keep} type="button" className="btn" onClick={onKeep}>
          Keep the change
        </button>
        <button type="button" className="btn danger" onClick={onUndo}>
          Undo
        </button>
      </div>
    </Modal>
  );
}

export const changeScope = (proposal: string) => `change:${proposal}`;

export function ChangeHistory({
  changes,
  doc,
  locked,
  refusalFor,
  onUndo,
}: {
  changes: MemoryChange[];
  // The document's name, as its tab names it.
  doc: string;
  locked: boolean;
  refusalFor: (scope: string) => ActionError | null;
  onUndo: (change: MemoryChange) => void;
}) {
  const [asking, setAsking] = useState<MemoryChange | null>(null);
  let rows: ReactNode;
  if (changes.length === 0) {
    rows = <p className="h-none">No changes in the last 30 days.</p>;
  } else {
    rows = (
      <ul className="h-list">
        {changes.map((change) => {
          const undone = change.undone_at !== null;
          const refusal = refusalFor(changeScope(change.proposal_id));
          return (
            <li key={change.proposal_id} data-kind={change.kind}>
              <span className="t">
                <span className={`lab sm${undone ? "" : TONE[change.kind]}`}>
                  {undone ? "Undone" : LABEL[change.kind]}
                </span>
                <ChangeText change={change} />
              </span>
              <span className="m">{meta(change)}</span>
              {undone ? (
                <span className="r">Already undone</span>
              ) : (
                <button
                  type="button"
                  className="btn sm"
                  disabled={locked}
                  aria-label={`Undo: ${LABEL[change.kind]} ${oneLine(change.text)}`}
                  onClick={() => setAsking(change)}
                >
                  Undo...
                </button>
              )}
              {refusal ? (
                <div className="h-refusal">
                  <Inline error={refusal} />
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    );
  }
  return (
    <section className="h-box" aria-labelledby="history-title">
      <div className="h-head">
        <h2 id="history-title">Change history</h2>
        <span className="c">{doc} · last 30 days</span>
      </div>
      {rows}
      {asking ? (
        <UndoDialog
          change={asking}
          doc={doc}
          onKeep={() => setAsking(null)}
          onUndo={() => {
            setAsking(null);
            onUndo(asking);
          }}
        />
      ) : null}
    </section>
  );
}
