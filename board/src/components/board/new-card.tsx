import { useCallback, useId, useRef, useState } from "react";

import type { MutationResponse, Project } from "../../lib/api-types";
import { LIMITS, isTooLong } from "../../lib/limits";
import { hostField, ownerOf } from "../../lib/peers";
import { ActionFailure } from "../attention/action-failure";
import { sendMutation } from "../attention/api";
import { PRIORITIES } from "../attention/format";
import { LimitNote } from "../attention/limit-note";
import { useAction } from "../attention/use-action";
import { ProjectDialog } from "../shell/project-dialog";
import { Select } from "../ui/select";
import "../shell/project-stage.css";
import { showToast } from "./toast";

// `tiller card create --kind`'s values, the one most cards are first.
const KINDS = ["ship", "scout", "question", "ops"] as const;

// The daemon's own priority when none is given: Medium.
const DEFAULT_PRIORITY = 2;

// What `POST /api/v1/cards` returns: the card filed, and `warning` when the
// project has no orchestrator to tell (protocol.md 2.2).
type Filed = { card?: { display_id?: string }; warning?: string | null } | null;

// The lane header's New card: the fields `tiller card create` takes. The
// card is filed through the board's own ask, so its orchestrator is told.
export function NewCardDialog({ project, onClose }: { project: Project; onClose: () => void }) {
  const [title, setTitle] = useState("");
  const [kind, setKind] = useState<string>(KINDS[0]);
  const [priority, setPriority] = useState(String(DEFAULT_PRIORITY));
  const [body, setBody] = useState("");
  const fields = useId();
  const first = useRef<HTMLInputElement>(null);

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Filed>("POST", "/api/v1/cards", {
        project: project.id,
        kind,
        title: title.trim(),
        body: body.trim() || null,
        priority: Number(priority),
        ...hostField(project),
        idempotency_key,
      }),
    [body, kind, priority, project, title],
  );
  const recorded = useCallback(
    ({ data }: MutationResponse<Filed>) => {
      onClose();
      const filed = data?.card?.display_id;
      const heading = filed ? `Filed ${filed}` : `Filed a card in ${project.name}`;
      if (data?.warning === "no_orchestrator") {
        showToast("ok", heading, "No orchestrator is running, so it is told when one starts.");
      } else {
        showToast("ok", heading, null);
      }
    },
    [onClose, project.name],
  );
  // Each set of values is its own click: an edit after a refusal is a new
  // card, while a retry of the same values reuses its key.
  const action = useAction({
    binding: `new-card:${project.id}:${kind}:${priority}:${title.trim()}:${body.trim()}`,
    send,
    onRecorded: recorded,
  });
  const over = isTooLong(title.trim(), LIMITS.cardTitle) || isTooLong(body.trim(), LIMITS.cardBody);
  const armed = title.trim() !== "" && !over && !action.pending;

  return (
    <ProjectDialog name="new-card" pending={action.pending} onClose={onClose} initial={first}>
      <h2 id="new-card-dialog-title">New card in {project.name}</h2>
      <p id="new-card-dialog-body">
        Files a card, as <code>tiller card create</code> does, and tells the project's orchestrator.
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (armed) action.trigger();
        }}
      >
        <div className="form-fields">
          <label htmlFor={`${fields}-title`}>Title</label>
          <input
            ref={first}
            id={`${fields}-title`}
            className="note-in"
            autoComplete="off"
            value={title}
            disabled={action.pending}
            onChange={(event) => setTitle(event.target.value)}
          />
          <label htmlFor={`${fields}-kind`}>Kind</label>
          <Select
            id={`${fields}-kind`}
            className="form-select"
            value={kind}
            disabled={action.pending}
            onValueChange={setKind}
            options={KINDS.map((k) => ({ value: k, label: k }))}
          />
          <label htmlFor={`${fields}-priority`}>Priority</label>
          <Select
            id={`${fields}-priority`}
            className="form-select"
            value={priority}
            disabled={action.pending}
            onValueChange={setPriority}
            options={PRIORITIES.map((label, index) => ({ value: String(index), label }))}
          />
          <label htmlFor={`${fields}-body`} className="at-top">
            Body
          </label>
          <textarea
            id={`${fields}-body`}
            className="note-in area"
            placeholder="Optional"
            value={body}
            disabled={action.pending}
            onChange={(event) => setBody(event.target.value)}
          />
        </div>
        <LimitNote what="title" text={title.trim()} limit={LIMITS.cardTitle} />
        <LimitNote what="body" text={body.trim()} limit={LIMITS.cardBody} />
        <ActionFailure error={action.error} host={ownerOf(project)} />
        <div className="dialog-actions">
          <button type="button" className="btn" disabled={action.pending} onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn primary" disabled={!armed}>
            {action.pending ? "Filing..." : "File card"}
          </button>
        </div>
      </form>
    </ProjectDialog>
  );
}
