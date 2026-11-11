import { Menu } from "@base-ui/react/menu";
import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";
import type { ReactNode } from "react";

import type {
  AgentControlBody,
  AgentRow,
  MutationResponse,
  PauseBody,
  PauseInfo,
  RelaunchBody,
  Resumed,
  SteerBody,
  SteerQueued,
} from "../../lib/api-types";
import { serverNow } from "../../lib/clock";
import { LIMITS, isTooLong } from "../../lib/limits";
import { lisbonDate, lisbonInstant } from "../../lib/lisbon-input";
import { agentsPath, queryKeyFor } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { PauseIcon, PlayIcon, ReopenIcon } from "../attention/icons";
import { LimitNote } from "../attention/limit-note";
import { ulid } from "../attention/ulid";
import { useAction } from "../attention/use-action";
import { Modal } from "../ui/modal";
import type { ActivityView } from "./activity";
import { formatLisbonMoment } from "./format";
import { ChevronDownIcon, PaneIcon, StopIcon } from "./icons";
import { SteerState } from "./steer-bar";
import { showToast } from "./toast";
import "../attention/card-lifecycle.css";
import "./run-menu.css";

// The line the paused agent's Resume sends, through the steer path.
const GO_ON = "go on";

function agentPath(sessionId: string, verb: string): string {
  return `/api/v1/agents/${encodeURIComponent(sessionId)}/${verb}`;
}

// Where an agent's run is, as the header's primary follows it: paused, a
// pane that is gone, or at work (protocol.md 7.4, P11).
export type RunState = "paused" | "dead" | "running";

export function runStateOf(agent: Pick<AgentRow, "activity">, activity: ActivityView): RunState {
  if (agent.activity === "ended" || agent.activity === "errored") return "dead";
  return activity.label === "Paused" ? "paused" : "running";
}

// What the Run menu acts on: one local agent.
export interface RunTarget {
  sessionId: string;
  name: string;
  // An orchestrator has no run: no pause, relaunch or resume.
  orchestrator: boolean;
  state: RunState;
  // A worker whose run is still open: core refuses to stop its agent until
  // the run ends, so the Stop dialog says so and sends nothing.
  runOpen: boolean;
}

export type RunDialog = "pause" | "relaunch" | "stop";

// Re-reads the agents list once a control is recorded, so the header follows
// at once; the change event does the same a moment later.
function useRefreshAgents(): () => void {
  const queryClient = useQueryClient();
  return useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
  }, [queryClient]);
}

// `POST .../resume`: `claude --resume` in the run's worktree for a pane that
// is gone. One click, no dialog: a live agent is left alone.
export function useResumeSession(target: RunTarget): () => void {
  const refresh = useRefreshAgents();
  return useCallback(() => {
    void sendMutation<Resumed>("POST", agentPath(target.sessionId, "resume"), {
      idempotency_key: ulid(),
    } satisfies AgentControlBody)
      .then((response) => {
        refresh();
        showToast(
          response.warning ? "bad" : "ok",
          response.data.left_alone
            ? `${target.name} is still running`
            : `Resumed ${target.name}'s session`,
          response.warning ??
            (response.data.left_alone
              ? "Its pane still runs, so nothing was started."
              : "It continues its Claude session in the same worktree."),
        );
      })
      .catch((error: unknown) => {
        showToast(
          "bad",
          `Could not resume ${target.name}`,
          error instanceof ActionError ? error.message : "Something went wrong.",
        );
      });
  }, [refresh, target.name, target.sessionId]);
}

// The Run menu left of Details: the run's rare controls, one click further
// than the primary. `onOpen` adds Open in herdr when the primary is not it,
// and `onGoOn` adds a paused run's Resume where no primary holds it.
export function RunMenu({
  target,
  onDialog,
  onResumeSession,
  onRestart,
  onOpen,
  onGoOn,
  small = false,
}: {
  target: RunTarget;
  onDialog: (dialog: RunDialog) => void;
  onResumeSession: () => void;
  onRestart?: () => void;
  onOpen?: () => void;
  onGoOn?: () => void;
  small?: boolean;
}) {
  return (
    <Menu.Root>
      <Menu.Trigger className={small ? "btn sm" : "btn"} aria-label="Run actions">
        Run
        <ChevronDownIcon className="icon" />
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner className="sel-pos" align="end" sideOffset={4}>
          <Menu.Popup className="cm-popup run-menu" aria-label="Run actions">
            {target.orchestrator ? (
              onRestart ? (
                <Item icon={<ReopenIcon />} onSelect={onRestart}>
                  Restart...
                </Item>
              ) : null
            ) : (
              <>
                {target.state === "paused" && onGoOn ? (
                  <Item icon={<PlayIcon />} onSelect={onGoOn}>
                    Resume
                  </Item>
                ) : null}
                <Item icon={<PauseIcon />} onSelect={() => onDialog("pause")}>
                  Pause until...
                </Item>
                <Item icon={<ReopenIcon />} onSelect={() => onDialog("relaunch")}>
                  Relaunch...
                </Item>
                {target.state === "dead" ? (
                  <Item icon={<PlayIcon />} onSelect={onResumeSession}>
                    Resume session
                  </Item>
                ) : null}
              </>
            )}
            {onOpen ? (
              <Item icon={<PaneIcon />} onSelect={onOpen}>
                Open in herdr
              </Item>
            ) : null}
            <Menu.Separator className="cm-sep" />
            <Item icon={<StopIcon />} bad onSelect={() => onDialog("stop")}>
              Stop agent...
            </Item>
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}

function Item({
  icon,
  bad,
  onSelect,
  children,
}: {
  icon: ReactNode;
  bad?: boolean;
  onSelect: () => void;
  children: ReactNode;
}) {
  return (
    <Menu.Item className={bad ? "cm-item bad" : "cm-item"} onClick={onSelect}>
      {icon}
      {children}
    </Menu.Item>
  );
}

// The dialog the owner opened from the Run menu or the primary, or nothing.
export function RunDialogs({
  target,
  dialog,
  onClose,
  onOpen,
  onStopped,
}: {
  target: RunTarget;
  dialog: RunDialog | null;
  onClose: () => void;
  // Opens the agent's pane in herdr; without it the dialogs offer no Open.
  onOpen?: () => void;
  // Called once a stop is recorded, before the dialog closes.
  onStopped?: (warning: string | null) => void;
}) {
  switch (dialog) {
    case "pause":
      return <PauseDialog target={target} onClose={onClose} />;
    case "relaunch":
      return <RelaunchDialog target={target} onClose={onClose} />;
    case "stop":
      return <StopDialog target={target} onClose={onClose} onOpen={onOpen} onStopped={onStopped} />;
    default:
      return null;
  }
}

function Field({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor: string;
  children: ReactNode;
}) {
  return (
    <div className="dlg-f">
      <label className="dlg-l" htmlFor={htmlFor}>
        {label}
      </label>
      {children}
    </div>
  );
}

// `run.pause` as `tiller run pause --until --reason`: the run's alarms stay
// quiet until the date, and the pane is kept.
function PauseDialog({ target, onClose }: { target: RunTarget; onClose: () => void }) {
  // Tomorrow morning, in Lisbon time, as a card's Hold starts.
  const [date, setDate] = useState(() => lisbonDate(serverNow(), 1));
  const [time, setTime] = useState("09:00");
  const [reason, setReason] = useState("");
  const until = lisbonInstant(date, time);
  const why = reason.trim();
  const refresh = useRefreshAgents();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", agentPath(target.sessionId, "pause"), {
        idempotency_key,
        until: until ?? "",
        reason: why,
      } satisfies PauseBody),
    [target.sessionId, until, why],
  );
  const onRecorded = useCallback(() => {
    refresh();
    onClose();
  }, [refresh, onClose]);
  const action = useAction({
    binding: `${target.sessionId}:pause:${until}:${why}`,
    send,
    onRecorded,
  });
  const keep = useRef<HTMLButtonElement>(null);
  const past = until !== null && Date.parse(until) <= serverNow();
  const over = isTooLong(why, LIMITS.comment);
  return (
    <Modal
      className="dialog run-dlg"
      labelledBy="pause-run-title"
      describedBy="pause-run-body"
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="pause-run-title">Pause {target.name}</h2>
      <p id="pause-run-body">
        The run&apos;s alarms stay quiet until the time below. The pane is kept, and Resume clears
        the pause sooner and sends the agent a &quot;go on&quot; line.
      </p>
      <Field label="Until (Lisbon time)" htmlFor="pause-date">
        <div className="dlg-when">
          <input
            id="pause-date"
            type="date"
            className="note-in"
            value={date}
            disabled={action.pending}
            onChange={(event) => setDate(event.target.value)}
          />
          <input
            id="pause-time"
            type="time"
            aria-label="Time"
            className="note-in"
            value={time}
            disabled={action.pending}
            onChange={(event) => setTime(event.target.value)}
          />
        </div>
        {past ? <div className="dlg-hint">That time has passed. Pick a later one.</div> : null}
      </Field>
      <Field label="Reason" htmlFor="pause-reason">
        <textarea
          id="pause-reason"
          className="note-in area"
          placeholder="What the run waits for"
          value={reason}
          disabled={action.pending}
          onChange={(event) => setReason(event.target.value)}
        />
      </Field>
      <LimitNote what="reason" text={why} limit={LIMITS.comment} />
      <ActionFailure error={action.error} host={undefined} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Cancel
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={until === null || past || why === "" || over || action.pending}
          onClick={action.trigger}
        >
          <PauseIcon />
          {action.pending ? "Pausing..." : "Pause"}
        </button>
      </div>
    </Modal>
  );
}

// `tiller agent relaunch --note [--new-attempt]`: a new pane on the same
// branch and worktree, which reads the note first.
function RelaunchDialog({ target, onClose }: { target: RunTarget; onClose: () => void }) {
  const [note, setNote] = useState("");
  const [attempt, setAttempt] = useState(false);
  const text = note.trim();
  const refresh = useRefreshAgents();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", agentPath(target.sessionId, "relaunch"), {
        idempotency_key,
        note: text,
        new_attempt: attempt,
      } satisfies RelaunchBody),
    [target.sessionId, text, attempt],
  );
  const onRecorded = useCallback(
    (response: MutationResponse<unknown>) => {
      refresh();
      showToast(
        response.warning ? "bad" : "ok",
        attempt ? `Relaunched ${target.name} as a new attempt` : `Relaunched ${target.name}`,
        response.warning ?? "The new pane reads your note first.",
      );
      onClose();
    },
    [refresh, onClose, attempt, target.name],
  );
  const action = useAction({
    binding: `${target.sessionId}:relaunch:${attempt}:${text}`,
    send,
    onRecorded,
  });
  const keep = useRef<HTMLButtonElement>(null);
  const over = isTooLong(text, LIMITS.comment);
  return (
    <Modal
      className="dialog run-dlg"
      labelledBy="relaunch-run-title"
      describedBy="relaunch-run-body"
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="relaunch-run-title">Relaunch {target.name}</h2>
      <p id="relaunch-run-body">
        A new pane starts on the same branch and worktree. The agent reads your note first.
      </p>
      <Field label="Note" htmlFor="relaunch-note">
        <textarea
          id="relaunch-note"
          className="note-in area"
          placeholder="Where the work is, and what to do next"
          value={note}
          disabled={action.pending}
          onChange={(event) => setNote(event.target.value)}
        />
      </Field>
      <label className="run-cbx">
        <input
          type="checkbox"
          checked={attempt}
          disabled={action.pending}
          onChange={() => setAttempt(!attempt)}
        />
        <span>
          Count as a new attempt
          <small>Leave it off when the pane died for a reason outside the work.</small>
        </span>
      </label>
      <LimitNote what="note" text={text} limit={LIMITS.comment} />
      <ActionFailure error={action.error} host={undefined} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Cancel
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={text === "" || over || action.pending}
          onClick={action.trigger}
        >
          <ReopenIcon />
          {action.pending ? "Relaunching..." : "Relaunch"}
        </button>
      </div>
    </Modal>
  );
}

// What a refused stop says, in the board's words where it can say what to do
// next; otherwise the daemon's own.
export function stopRefusal(error: ActionError): string {
  const message = error.message;
  if (/holds a draft/.test(message)) {
    return "Not stopped: the pane has an unsent draft. Open it in herdr and send or clear the draft, then stop again.";
  }
  if (/shows a dialog/.test(message)) {
    return "Not stopped: the agent is waiting on a dialog. Answer it first.";
  }
  if (/is active; end it first/.test(message)) {
    return "Not stopped: its run is still open. The agent ends the run when its work is done, or Cancel card on the card page stops it.";
  }
  return message;
}

// `tiller agent stop`: a proven stop, then the session ends. A worker's run
// must have ended first; core refuses while it is open.
function StopDialog({
  target,
  onClose,
  onOpen,
  onStopped,
}: {
  target: RunTarget;
  onClose: () => void;
  onOpen: (() => void) | undefined;
  onStopped: ((warning: string | null) => void) | undefined;
}) {
  const refresh = useRefreshAgents();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", agentPath(target.sessionId, "stop"), {
        idempotency_key,
      } satisfies AgentControlBody),
    [target.sessionId],
  );
  const onRecorded = useCallback(
    (response: MutationResponse<unknown>) => {
      refresh();
      showToast(
        response.warning ? "bad" : "ok",
        `Stopped ${target.name}`,
        response.warning ?? "Its pane is closed and its session ended.",
      );
      onStopped?.(response.warning ?? null);
      onClose();
    },
    [refresh, onClose, onStopped, target.name],
  );
  // Each dialog is one press: its key is kept for a retry after a refusal.
  const [press] = useState(() => `${target.sessionId}:stop:${Date.now()}`);
  const action = useAction({ binding: press, send, onRecorded });
  const keep = useRef<HTMLButtonElement>(null);
  const refused = action.error && action.error.status === 422 ? action.error : null;
  if (target.runOpen && !target.orchestrator) {
    return (
      <Modal
        className="dialog run-dlg"
        labelledBy="stop-agent-title"
        describedBy="stop-agent-body"
        onClose={onClose}
        initialFocus={keep}
      >
        <h2 id="stop-agent-title">{target.name} is still on its run</h2>
        <p id="stop-agent-body">
          Its agent stops only once the run has ended. The agent ends the run when its work is done,
          or Cancel card on the card page ends it and stops the agent.
        </p>
        <div className="dialog-actions">
          {onOpen ? (
            <button type="button" className="btn" onClick={onOpen}>
              <PaneIcon />
              Open in herdr
            </button>
          ) : null}
          <button ref={keep} type="button" className="btn" onClick={onClose}>
            Close
          </button>
        </div>
      </Modal>
    );
  }
  return (
    <Modal
      className="dialog run-dlg"
      labelledBy="stop-agent-title"
      describedBy="stop-agent-body"
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="stop-agent-title">Stop {target.name}?</h2>
      <p id="stop-agent-body">
        {target.orchestrator
          ? "The pane closes and the orchestrator's session ends. Its builders keep working, and Start orchestrator on the lane brings it back."
          : "The pane closes and the session ends. The branch and worktree stay on disk. The agent stops only once its run has ended."}
      </p>
      {refused ? (
        <p className="refusal" role="alert">
          {stopRefusal(refused)}
        </p>
      ) : (
        <ActionFailure error={action.error} host={undefined} />
      )}
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Keep running
        </button>
        {onOpen ? (
          <button type="button" className="btn" disabled={action.pending} onClick={onOpen}>
            <PaneIcon />
            Open in herdr
          </button>
        ) : null}
        <button
          type="button"
          className="btn danger"
          disabled={action.pending}
          onClick={action.trigger}
        >
          <StopIcon />
          {action.pending ? "Stopping..." : "Stop agent"}
        </button>
      </div>
    </Modal>
  );
}

// A paused run's note under the header: until when, why, and who paused it.
// After Resume it says where the "go on" line is.
export function PausedNote({ pause, receiptKey }: { pause: PauseInfo; receiptKey: string | null }) {
  const until = formatLisbonMoment(pause.until) ?? pause.until;
  const by = pause.by === "board" || pause.by === "owner" ? "you" : pause.by;
  return (
    <div className="run-paused">
      <div className="ch-note" role="status">
        <PauseIcon />
        <div>
          <b>Paused until {until}.</b>
          {pause.reason ? ` ${pause.reason}` : null}
          <div className="sub">
            Paused by {by}, {formatLisbonMoment(pause.at) ?? pause.at}. Its alarms stay quiet until
            that time.
          </div>
        </div>
      </div>
      {receiptKey ? (
        <div className="av-steer-st">
          <SteerState receiptKey={receiptKey} text={GO_ON} />
        </div>
      ) : (
        <p className="run-note">
          The pane is kept. Resume clears the pause and sends the agent a &quot;go on&quot; line.
          Run holds Relaunch and Stop.
        </p>
      )}
    </div>
  );
}

// After Resume cleared the pause: the run's alarms count again, and where
// the "go on" line is.
export function ResumedNote({ receiptKey }: { receiptKey: string }) {
  return (
    <div className="run-paused">
      {/* The accent tone: a cleared pause is settled, not a warning. */}
      <div className="ch-note closed" role="status">
        <PlayIcon />
        <div>
          <b>Resumed. The pause is cleared.</b>
          <div className="sub">Its alarms count again.</div>
        </div>
      </div>
      <div className="av-steer-st">
        <SteerState receiptKey={receiptKey} text={GO_ON} />
      </div>
    </div>
  );
}

// The paused agent's primary: clear the pause (`run.unpause`), then one
// "go on" line through the steer path.
export function useGoOn(sessionId: string, pause: PauseInfo | null | undefined) {
  // The receipt is the session's: the Stage stays mounted when the owner
  // picks another agent, and one agent's go on must not hold the next one's.
  const [receipt, setReceipt] = useState<{ sessionId: string; key: string } | null>(null);
  const refresh = useRefreshAgents();
  const send = useCallback(
    async (idempotency_key: string) => {
      // Each mutation keeps its own key, so a retry replays both.
      await sendMutation("POST", agentPath(sessionId, "unpause"), {
        idempotency_key: `${idempotency_key}-unpause`,
      } satisfies AgentControlBody);
      return sendMutation<SteerQueued>("POST", agentPath(sessionId, "steer"), {
        idempotency_key,
        text: GO_ON,
      } satisfies SteerBody);
    },
    [sessionId],
  );
  const onRecorded = useCallback(
    (_: MutationResponse<SteerQueued>, key: string) => {
      setReceipt({ sessionId, key });
      refresh();
    },
    [refresh, sessionId],
  );
  const action = useAction({
    binding: `${sessionId}:go-on:${pause?.at ?? ""}`,
    send,
    onRecorded,
  });
  return { ...action, sent: receipt?.sessionId === sessionId ? receipt.key : null };
}
