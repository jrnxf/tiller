import { useQuery } from "@tanstack/react-query";
import { useCallback, useId, useRef, useState } from "react";

import type { MutationResponse, Project } from "../../lib/api-types";
import { LIMITS, isTooLong } from "../../lib/limits";
import { hostField, ownerOf, projectKey, standingOf } from "../../lib/peers";
import { prefsStore, visibleProjects } from "../../lib/prefs";
import { projectsQuery } from "../../lib/queries";
import { createStore, useStore } from "../../lib/store";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { PRIORITIES } from "../attention/format";
import { LimitNote } from "../attention/limit-note";
import { useAction } from "../attention/use-action";
import { ProjectDialog } from "../shell/project-dialog";
import { Select } from "../ui/select";
import "../shell/project-stage.css";
import { useMachineRows, useNamedProjects } from "./peer-state";
import { showToast } from "./toast";

// The project (its project key) a New card dialog is open for, when the
// header, the `C` key or the command palette opened it. A lane's own button
// keeps its own dialog.
export const newCardStore = createStore<string | null>(null);

// The project a new card goes to with no question asked: the one the view is
// focused on, when the owner may act on it. In "All projects" there is none.
export function useNewCardTarget(): Project | null {
  const projects = useNamedProjects(useQuery(projectsQuery()).data)?.projects;
  const machines = useMachineRows();
  const prefs = useStore(prefsStore);
  if (prefs.view !== "focus" || !projects) return null;
  const focused = visibleProjects(prefs, projects)[0];
  return focused && standingOf(focused, machines).canAct ? focused : null;
}

// The dialog `newCardStore` asks for. It closes itself when the project is
// gone from the list.
export function GlobalNewCard() {
  const key = useStore(newCardStore);
  const projects = useNamedProjects(useQuery(projectsQuery()).data)?.projects;
  const close = useCallback(() => newCardStore.set(null), []);
  const project = key === null ? undefined : projects?.find((p) => projectKey(p) === key);
  return project ? <NewCardDialog key={key} project={project} onClose={close} /> : null;
}

// `tiller card create --kind`'s values, the one most cards are first.
const KINDS = ["ship", "scout", "question", "ops"] as const;

// The daemon's own priority when none is given: Medium.
const DEFAULT_PRIORITY = 2;

// What `POST /api/v1/cards` returns: the card filed, and `warning` when the
// project has no orchestrator to tell (protocol.md 2.2).
type Filed = { card?: { id?: string; display_id?: string }; warning?: string | null } | null;

// The kinds that have a run to start: `tiller spawn` takes a builder for a
// ship card and a scout for a scout card (protocol.md 7.4).
function startsRun(kind: string): boolean {
  return kind === "ship" || kind === "scout";
}

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
  // File, then start the run of the card as `tiller spawn` does. The card
  // is saved before the start is tried. A refusal of the start says so and
  // names its reason, and a retry replays the filing with the same key, so
  // it never files a second card.
  const sendAndStart = useCallback(
    async (idempotency_key: string) => {
      const filed = await send(idempotency_key);
      const id = filed.data?.card?.id ?? filed.data?.card?.display_id;
      const shown = filed.data?.card?.display_id ?? "The card";
      if (!id)
        throw new ActionError(0, "internal", `${shown} was filed, but it has no id to start.`);
      try {
        await sendMutation("POST", `/api/v1/cards/${id}/runs`, {
          ...hostField(project),
          idempotency_key: `${idempotency_key}:start`,
        });
      } catch (caught) {
        if (caught instanceof ActionError) {
          throw new ActionError(
            caught.status,
            caught.code,
            `${shown} is filed, but no run started: ${caught.message}`,
            caught.current ?? undefined,
            caught.hint ?? undefined,
            caught.host ?? undefined,
          );
        }
        throw caught;
      }
      return filed;
    },
    [project, send],
  );
  const recorded = useCallback(
    ({ data }: MutationResponse<Filed>, started = false) => {
      onClose();
      const filed = data?.card?.display_id;
      const heading = filed
        ? `${started ? "Filed and started" : "Filed"} ${filed}`
        : `Filed a card in ${project.name}`;
      if (data?.warning === "no_orchestrator" && !started) {
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
    onRecorded: (response) => recorded(response),
  });
  const start = useAction({
    binding: `new-card-start:${project.id}:${kind}:${priority}:${title.trim()}:${body.trim()}`,
    send: sendAndStart,
    onRecorded: (response) => recorded(response, true),
  });
  const pending = action.pending || start.pending;
  const over = isTooLong(title.trim(), LIMITS.cardTitle) || isTooLong(body.trim(), LIMITS.cardBody);
  const armed = title.trim() !== "" && !over && !pending;

  return (
    <ProjectDialog name="new-card" pending={pending} onClose={onClose} initial={first}>
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
            disabled={pending}
            onChange={(event) => setTitle(event.target.value)}
          />
          <label htmlFor={`${fields}-kind`}>Kind</label>
          <Select
            id={`${fields}-kind`}
            className="form-select"
            value={kind}
            disabled={pending}
            onValueChange={setKind}
            options={KINDS.map((k) => ({ value: k, label: k }))}
          />
          <label htmlFor={`${fields}-priority`}>Priority</label>
          <Select
            id={`${fields}-priority`}
            className="form-select"
            value={priority}
            disabled={pending}
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
            disabled={pending}
            onChange={(event) => setBody(event.target.value)}
          />
        </div>
        <LimitNote what="title" text={title.trim()} limit={LIMITS.cardTitle} />
        <LimitNote what="body" text={body.trim()} limit={LIMITS.cardBody} />
        <ActionFailure error={action.error} host={ownerOf(project)} />
        <ActionFailure error={start.error} host={ownerOf(project)} proof />
        <div className="dialog-actions">
          <button type="button" className="btn" disabled={pending} onClick={onClose}>
            Cancel
          </button>
          {startsRun(kind) ? (
            <button
              type="button"
              className="btn"
              disabled={!armed}
              title="Files the card and starts its run, as tiller spawn does"
              onClick={start.trigger}
            >
              {start.pending ? "Starting..." : "File and start"}
            </button>
          ) : null}
          <button type="submit" className="btn primary" disabled={!armed}>
            {action.pending ? "Filing..." : "File card"}
          </button>
        </div>
      </form>
    </ProjectDialog>
  );
}
