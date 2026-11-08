import { useQuery } from "@tanstack/react-query";
import type { Editor } from "@tiptap/core";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { createPortal } from "react-dom";

import type { Project } from "../../lib/api-types";
import { askFor, mentionedKeys, messageFor } from "../../lib/ask-plan";
import type { AskPiece } from "../../lib/ask-plan";
import { isFederated, ownerOf, projectKey, standingOf } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { agentsQuery, machinesQuery, projectsQuery } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { SendIcon } from "../attention/icons";
import { showToast } from "../board/toast";
import { AskEditor, removeMention } from "./ask-editor";
import type { MentionOption } from "./ask-editor";
import { useAskSends } from "./ask-send";
import type { AskTarget, Outcome } from "./ask-send";
import "./ask-dialog.css";
import { useNamedProjects } from "../board/peer-state";

const ASK_MAX_CHARS = 8000;

// The toast once every ask is sent.
function sentToast(targets: AskTarget[], outcomes: Record<string, Outcome>) {
  const started = (target: AskTarget) => {
    const outcome = outcomes[projectKey(target.project)];
    return outcome?.status === "sent" && outcome.started;
  };
  if (targets.length === 1 && targets[0]) {
    const [only] = targets;
    showToast(
      "ok",
      `Sent to the ${only.project.name} orchestrator`,
      started(only) ? "Its orchestrator was not running, so one was started." : null,
    );
    return;
  }
  const startedCount = targets.filter(started).length;
  showToast(
    "ok",
    `Sent ${targets.length} asks`,
    startedCount > 0
      ? `${startedCount} orchestrator${startedCount === 1 ? " was" : "s were"} started first.`
      : null,
  );
}

// What a row of the summary says about its project. Before the send it is the
// orchestrator's state; after, how that project's send ended.
function statusOf(
  running: boolean,
  outcome: Outcome | undefined,
): { text: string; tone: "ok" | "warn" | "fail" } {
  if (outcome?.status === "sending") return { text: "Sending...", tone: "ok" };
  if (outcome?.status === "sent") {
    return { text: outcome.started ? "Orchestrator started, ask sent" : "Sent", tone: "ok" };
  }
  if (outcome?.status === "failed") return { text: "Could not send", tone: "fail" };
  return running
    ? { text: "orchestrator running", tone: "ok" }
    : { text: "will start orchestrator", tone: "warn" };
}

// One project the ask goes to, with the state of its orchestrator and, once
// the send has begun, how it went.
function SendRow({
  project,
  color,
  running,
  where,
  outcome,
  off,
  onRemove,
}: {
  project: Project;
  color: string;
  running: boolean;
  where: string | null;
  outcome: Outcome | undefined;
  off: string | null;
  onRemove: (() => void) | null;
}) {
  const status = statusOf(running, outcome);
  return (
    <li style={{ "--pc": color } as CSSProperties} data-testid={`ask-row-${project.name}`}>
      <span className="ask-dot" />
      <span className="ask-row-name">{project.name}</span>
      {where ? <span className="ask-tag">{where}</span> : null}
      <span className="ask-row-state" data-tone={off ? "fail" : status.tone}>
        {off ?? status.text}
      </span>
      {onRemove ? (
        <button
          type="button"
          className="ask-remove"
          aria-label={`Remove ${project.name}`}
          onClick={onRemove}
        >
          ×
        </button>
      ) : null}
      {outcome?.status === "failed" ? (
        <div className="ask-row-note">
          <ActionFailure error={outcome.error} host={ownerOf(project)} />
        </div>
      ) : null}
    </li>
  );
}

function AskBody({ projects, onClose }: { projects: Project[]; onClose: () => void }) {
  const [opener] = useState(() => document.activeElement);
  const orchestrators = useQuery(agentsQuery()).data?.orchestrators;
  const machines = useQuery(machinesQuery()).data?.machines;
  const [pieces, setPieces] = useState<AskPiece[]>([]);
  const { outcomes, send } = useAskSends();
  const titleId = useId();
  const labelId = useId();
  const dialog = useRef<HTMLDivElement>(null);
  const editor = useRef<Editor | null>(null);
  // Escape closes an open list first and the dialog only when no list was open.
  const listOpen = useRef(false);
  const escapeClosesList = useRef(false);

  const federated = isFederated(machines ?? []);
  const options: MentionOption[] = useMemo(
    () =>
      projects.map((project, index) => {
        const standing = standingOf(project, machines ?? []);
        return {
          key: projectKey(project),
          name: project.name,
          color: projectColor(index),
          host: ownerOf(project) ?? null,
          off: standing.canAct ? null : standing.reason,
        };
      }),
    [projects, machines],
  );

  const message = messageFor(pieces);
  const picked = mentionedKeys(pieces).flatMap((key) => {
    const at = projects.findIndex((project) => projectKey(project) === key);
    const project = projects[at];
    return project ? [{ project, color: projectColor(at), off: options[at]?.off ?? null }] : [];
  });
  const targets: AskTarget[] = picked.map(({ project }) => ({
    project,
    running: (orchestrators ?? []).some(
      (agent) => agent.project_id === project.id && (agent.host ?? undefined) === ownerOf(project),
    ),
    text: askFor(
      message,
      picked.filter((other) => other.project !== project).map((other) => other.project.name),
    ),
  }));

  const attempted = Object.keys(outcomes).length > 0;
  const sending = Object.values(outcomes).some((outcome) => outcome.status === "sending");
  const failed = targets.filter(
    (target) => outcomes[projectKey(target.project)]?.status === "failed",
  );
  const tooLong = targets.some((target) => target.text.length > ASK_MAX_CHARS);
  const blocked = picked.some((entry) => entry.off !== null);
  const canSend =
    !sending &&
    (attempted ? failed.length > 0 : targets.length > 0 && message !== "" && !tooLong && !blocked);

  const submit = async () => {
    if (!canSend) return;
    // The first press sends to every project; a press after a failure only to
    // the ones that failed.
    const batch = attempted ? failed : targets;
    const landed = await send(batch);
    const now = { ...outcomes, ...landed };
    if (targets.every((target) => now[projectKey(target.project)]?.status === "sent")) {
      sentToast(targets, now);
      onClose();
    }
  };

  useEffect(() => {
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  }, [opener]);

  const sendLabel = sending
    ? "Sending..."
    : attempted
      ? `Retry ${failed.length} failed`
      : targets.length > 1
        ? `Send to ${targets.length}`
        : "Send";

  let hint: string | null = null;
  if (!attempted) {
    if (targets.length === 0) {
      hint =
        message === ""
          ? "Type @ to pick the projects this goes to."
          : "Add a project with @ first: this ask has nowhere to go.";
    } else if (message === "") {
      hint = "Say what they should do.";
    } else if (tooLong) {
      hint = `The ask is longer than ${ASK_MAX_CHARS} characters.`;
    }
  }

  return createPortal(
    <div className="dialog-backdrop" onMouseDown={() => !sending && onClose()}>
      <div
        ref={dialog}
        className="dialog ask-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDownCapture={(event) => {
          if (event.key === "Escape") escapeClosesList.current = listOpen.current;
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!sending && !escapeClosesList.current) onClose();
          } else if (event.key === "Tab") {
            // Keep focus on the dialog's own controls that can take it.
            const stops = [
              ...(dialog.current?.querySelectorAll<HTMLElement>(
                'button:not(:disabled):not([tabindex="-1"]), [contenteditable="true"]',
              ) ?? []),
            ];
            const [first, last] = [stops[0], stops.at(-1)];
            if (!first) {
              event.preventDefault();
            } else if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last?.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first.focus();
            }
          }
        }}
      >
        <h2 id={titleId}>Ask orchestrators</h2>
        <div className="ask-field">
          <span className="ask-label" id={labelId}>
            Ask
          </span>
          <AskEditor
            options={options}
            used={picked.map((entry) => projectKey(entry.project))}
            locked={attempted}
            placeholder="Type @ to pick projects, then say what they should do"
            labelledBy={labelId}
            editorRef={editor}
            onChange={setPieces}
            onSubmit={() => void submit()}
            onListOpen={(open) => {
              listOpen.current = open;
            }}
          />
          <div className="ask-sends" role="status" aria-live="polite">
            {targets.length > 0 ? (
              <>
                <div className="ask-sends-title">
                  {attempted
                    ? "Sends"
                    : `Sends to ${targets.length} project${targets.length === 1 ? "" : "s"}`}
                </div>
                <ul>
                  {picked.map(({ project, color, off }, index) => (
                    <SendRow
                      key={projectKey(project)}
                      project={project}
                      color={color}
                      running={targets[index]?.running ?? false}
                      where={federated ? `on ${ownerOf(project) ?? "this host"}` : null}
                      outcome={outcomes[projectKey(project)]}
                      off={off}
                      onRemove={
                        attempted
                          ? null
                          : () => {
                              if (editor.current)
                                removeMention(editor.current, projectKey(project));
                            }
                      }
                    />
                  ))}
                </ul>
              </>
            ) : null}
            {hint ? (
              <p className="ask-hint-line" data-warn={message !== "" && targets.length === 0}>
                {hint}
              </p>
            ) : null}
          </div>
        </div>
        <div className="dialog-actions">
          <span className="ask-hint">Enter sends, Shift+Enter adds a line</span>
          <button type="button" className="btn" disabled={sending} onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn primary"
            disabled={!canSend}
            onClick={() => void submit()}
          >
            <SendIcon />
            {sendLabel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

// Mounted only while open, so each opening starts with nothing picked.
export function AskDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const projects = useNamedProjects(useQuery(projectsQuery()).data)?.projects;
  if (!open || !projects) return null;
  return <AskBody projects={projects} onClose={onClose} />;
}
