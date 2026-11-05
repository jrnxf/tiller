import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { QueryClient } from "@tanstack/react-query";
import type { CSSProperties } from "react";
import { createPortal } from "react-dom";

import type { AskedRow, AskRecorded, BoardResponse, Project } from "../../lib/api-types";
import { ownerOf, projectKey } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { agentsQuery, API_KEY_PREFIX, projectsQuery } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { sendMutation } from "../attention/api";
import { SendIcon } from "../attention/icons";
import { useAction } from "../attention/use-action";
import { showToast } from "../board/toast";
import "./ask-dialog.css";

const BOARD_PATH = "/api/v1/board";
const ASK_MAX_CHARS = 8000;

// The new ask joins the Asked column from the response, before the change
// event reaches the board. Only the reads that list this project's asks take it.
function showAsk(queryClient: QueryClient, row: AskedRow) {
  for (const [key, board] of queryClient.getQueriesData<BoardResponse>({
    queryKey: API_KEY_PREFIX,
  })) {
    const path = String(key[1] ?? "");
    if (!board || !path.startsWith(BOARD_PATH)) continue;
    const params = new URL(path, "http://localhost").searchParams;
    const project = params.get("project");
    if (params.get("host") || (project && project !== row.project_id)) continue;
    if (board.asked.some((ask) => ask.id === row.id)) continue;
    queryClient.setQueryData<BoardResponse>(key, { ...board, asked: [...board.asked, row] });
  }
}

// What the board tells the owner once the ask is recorded. With no
// orchestrator running the ask is still on the board, and the next one is told
// about it when it starts.
function receiptLine(project: Project, recorded: AskRecorded): [string, string | null] {
  return recorded.warning === "no_orchestrator"
    ? [
        "Ask recorded",
        `No orchestrator is running for ${project.name}: it will pick this up when it starts.`,
      ]
    : [`Sent to the ${project.name} orchestrator`, null];
}

// The board host does not forward an ask (protocol.md section 7.4), so a
// project another host owns cannot be picked here.
function offReason(project: Project): string | null {
  const owner = ownerOf(project);
  return owner ? `${owner} owns this project: ask its orchestrator there` : null;
}

interface PickerProps {
  projects: Project[];
  picked: string | null;
  disabled: boolean;
  onPick: (key: string) => void;
}

// The orchestrator picker: pills in one tab stop. Arrows move between the
// ones that can take an ask, Enter or Space (or the pill's number) picks, and
// Tab moves on to the text. Nothing is picked until the owner picks it.
function Picker({ projects, picked, disabled, onPick }: PickerProps) {
  const pickable = projects.filter((project) => offReason(project) === null);
  const [cursor, setCursor] = useState<string | null>(null);
  const refs = useRef(new Map<string, HTMLButtonElement>());
  const stop = picked ?? cursor ?? (pickable[0] ? projectKey(pickable[0]) : null);

  const move = (from: string, step: number) => {
    const at = pickable.findIndex((project) => projectKey(project) === from);
    const next = pickable[(at + step + pickable.length) % pickable.length];
    if (!next) return;
    setCursor(projectKey(next));
    refs.current.get(projectKey(next))?.focus();
  };

  return (
    <div className="ask-picker" role="radiogroup" aria-label="Orchestrator">
      {projects.map((project, position) => {
        const key = projectKey(project);
        const off = offReason(project);
        return (
          <button
            key={key}
            ref={(element) => {
              if (element) refs.current.set(key, element);
              else refs.current.delete(key);
            }}
            type="button"
            role="radio"
            className="ask-pill"
            aria-checked={picked === key}
            aria-disabled={off !== null || undefined}
            disabled={disabled}
            tabIndex={key === stop && off === null ? 0 : -1}
            title={off ?? undefined}
            data-off={off !== null}
            onClick={() => {
              if (off === null) onPick(key);
            }}
            onFocus={() => {
              if (off === null) setCursor(key);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowRight" || event.key === "ArrowDown") {
                event.preventDefault();
                move(key, 1);
              } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
                event.preventDefault();
                move(key, -1);
              } else if (/^[1-9]$/.test(event.key) && Number(event.key) === position + 1) {
                event.preventDefault();
                if (off === null) onPick(key);
              }
            }}
          >
            <span className="pdot" style={{ "--pc": projectColor(position) } as CSSProperties} />
            {project.name}
            <kbd>{position + 1}</kbd>
            {off !== null ? (
              <span className="ask-pill-off">{ownerOf(project)}, not pickable</span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

function AskBody({ projects, onClose }: { projects: Project[]; onClose: () => void }) {
  const [opener] = useState(() => document.activeElement);
  const queryClient = useQueryClient();
  const orchestrators = useQuery(agentsQuery()).data?.orchestrators;
  const [pickedKey, setPickedKey] = useState<string | null>(null);
  const [text, setText] = useState("");
  // A new click per ask: the same words can go to the same project twice.
  const [round, setRound] = useState(0);
  const titleId = useId();
  const dialog = useRef<HTMLDivElement>(null);

  const project = projects.find((candidate) => projectKey(candidate) === pickedKey) ?? null;
  const body = text.trim();
  const running = project
    ? (orchestrators ?? []).some((agent) => agent.project_id === project.id && !agent.host)
    : null;

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<AskRecorded>("POST", `/api/v1/projects/${project?.id}/asks`, {
        text: body,
        idempotency_key,
      }),
    [project?.id, body],
  );
  const onRecorded = useCallback(
    (response: { data: AskRecorded }) => {
      const { ask } = response.data;
      showAsk(queryClient, {
        id: ask.id,
        project_id: ask.project_id,
        session_id: ask.session_id,
        received_at: ask.received_at,
        age_seconds: 0,
      });
      if (project) {
        const [title, detail] = receiptLine(project, response.data);
        showToast("ok", title, detail);
      }
      setRound((n) => n + 1);
      onClose();
    },
    [onClose, project, queryClient],
  );
  const action = useAction<AskRecorded>({
    binding: `${round}:${pickedKey}:${body}`,
    send,
    onRecorded,
  });
  const ready = project !== null && body !== "" && !action.pending;
  const submit = () => {
    if (ready) action.trigger();
  };

  // Focus starts on the picker, so the orchestrator is chosen first, and goes
  // back to the Ask button on close.
  useEffect(() => {
    dialog.current?.querySelector<HTMLElement>('.ask-pill[tabindex="0"]:not(:disabled)')?.focus();
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  }, [opener]);

  const note = project
    ? running === false
      ? `No orchestrator is running for ${project.name}: the ask is recorded and picked up when it starts.`
      : `Goes to the ${project.name} orchestrator.`
    : "Pick an orchestrator first.";

  return createPortal(
    <div className="dialog-backdrop" onMouseDown={() => !action.pending && onClose()}>
      <div
        ref={dialog}
        className="dialog ask-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!action.pending) onClose();
          } else if (event.key === "Tab") {
            // Keep focus on the dialog's own controls that can take it.
            const stops = [
              ...(dialog.current?.querySelectorAll<HTMLElement>(
                'button:not(:disabled):not([tabindex="-1"]), textarea:not(:disabled)',
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
        <h2 id={titleId}>Ask an orchestrator</h2>
        <div className="ask-field">
          <span className="ask-label">Orchestrator</span>
          <Picker
            projects={projects}
            picked={pickedKey}
            disabled={action.pending}
            onPick={setPickedKey}
          />
        </div>
        <div className="ask-field">
          <label className="ask-label" htmlFor={`${titleId}-text`}>
            Ask
          </label>
          <textarea
            id={`${titleId}-text`}
            className="ask-in"
            rows={4}
            maxLength={ASK_MAX_CHARS}
            placeholder={project ? `Ask the ${project.name} orchestrator` : "What should it do?"}
            value={text}
            readOnly={action.pending}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              // Enter sends; Shift+Enter adds a line.
              if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault();
                submit();
              }
            }}
          />
          <div className="ask-note" role="status" aria-live="polite" data-warn={running === false}>
            {action.error ? null : note}
          </div>
          <div className="ask-fail">
            <ActionFailure error={action.error} host={project ? ownerOf(project) : undefined} />
          </div>
        </div>
        <div className="dialog-actions">
          <span className="ask-hint">Enter sends, Shift+Enter adds a line</span>
          <button type="button" className="btn" disabled={action.pending} onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn primary" disabled={!ready} onClick={submit}>
            <SendIcon />
            {action.pending ? "Sending..." : "Send"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

// Mounted only while open, so each opening starts with nothing picked.
export function AskDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const projects = useQuery(projectsQuery()).data?.projects;
  if (!open || !projects) return null;
  return <AskBody projects={projects} onClose={onClose} />;
}
