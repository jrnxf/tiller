import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";
import type { MouseEvent, RefObject } from "react";

import type { MutationResponse, SettingProblem } from "../../lib/api-types";
import { agentsPath, queryKeyFor, startSettingsQuery } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { ReopenIcon } from "../attention/icons";
import { ulid } from "../attention/ulid";
import { useAction } from "../attention/use-action";
import { settingsOf, StartSettingsFix } from "../shell/start-settings-fix";
import { Modal } from "../ui/modal";
import { showToast } from "./toast";
import "./run-menu.css";

export const startPath = (projectId: string) => `/api/v1/projects/${projectId}/orchestrator`;

// What the daemon returns: the started or live orchestrator (protocol.md 7.4).
type Started = { warning?: string | null } | null;

// A refusal over settings lists every one (`settings`), with the peer that
// refused (`host`) when the start ran there.
type StartResult =
  | { ok: true; replayed: boolean; warning: string | null }
  | { ok: false; message: string; settings: SettingProblem[]; host: string | undefined };

// A refusal that is not about settings.
const failed = (message: string): StartResult => ({
  ok: false,
  message,
  settings: [],
  host: undefined,
});

// `POST /api/v1/projects/{id}/orchestrator`: the same start as `tiller
// orchestrator start`, forwarded to `host` for a peer's project. A live
// orchestrator comes back replayed, never a second one, so each click is a
// fresh key. A refusal is shown with the daemon's own words (a missing checkout
// names the setting, an unreachable terminal holder says so), except where the board can
// say more about what to do.
async function startOrchestrator(projectId: string, host?: string): Promise<StartResult> {
  try {
    const response = await sendMutation<Started>("POST", startPath(projectId), {
      idempotency_key: ulid(),
      ...(host ? { host } : {}),
    });
    return { ok: true, replayed: response.replayed, warning: response.data?.warning ?? null };
  } catch (error) {
    if (!(error instanceof ActionError)) throw error;
    if (error.status === 0) {
      return failed("The daemon did not answer, so no orchestrator started.");
    }
    // This host's 403 is the cookie, unless its owner marker names another
    // system; that refusal, and a peer's own (it names the peer), is shown
    // in its words.
    if (error.status === 403 && error.host === null && error.code !== "owner_marker") {
      return failed(
        "This browser is not signed in as the owner. Run `tiller board --login` for a login link.",
      );
    }
    return {
      ok: false,
      message: error.message,
      settings: settingsOf(error),
      host: error.host ?? host,
    };
  }
}

// `restart: true` on the same route: `tiller orchestrator start --restart`
// as the board, an explicit owner act behind a confirmation (protocol.md 7.4).
// It runs on this host only, so a peer's lane never offers it.
export function RestartOrchestratorDialog({
  projectId,
  projectName,
  silent,
  onClose,
}: {
  projectId: string;
  projectName: string;
  // The lane saw the orchestrator go quiet, so the dialog says so.
  silent: boolean;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Started>("POST", startPath(projectId), { idempotency_key, restart: true }),
    [projectId],
  );
  const onRecorded = useCallback(
    (response: MutationResponse<Started>) => {
      void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
      const warning = response.data?.warning ?? response.warning ?? null;
      showToast(
        warning ? "bad" : "ok",
        `Restarted the ${projectName} orchestrator`,
        warning ?? "It shows in the lane header once its pane starts.",
      );
      onClose();
    },
    [queryClient, projectName, onClose],
  );
  // Each dialog is one press: its key is kept for a retry after a failure.
  const [press] = useState(() => `${projectId}:restart:${Date.now()}`);
  const action = useAction({ binding: press, send, onRecorded });
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <Modal
      className="dialog run-dlg"
      labelledBy="restart-orch-title"
      describedBy="restart-orch-body"
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="restart-orch-title">Restart the {projectName} orchestrator?</h2>
      <p id="restart-orch-body">
        {silent ? "The silent session" : "Its session"} is stopped and a new one starts with the
        same orders. Builders keep working.
      </p>
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
          disabled={action.pending || action.recorded}
          onClick={action.trigger}
        >
          <ReopenIcon />
          {action.pending ? "Restarting..." : "Restart"}
        </button>
      </div>
    </Modal>
  );
}

// The lane header's Restart..., beside the status of an orchestrator that
// went silent. It only asks: the lane renders the dialog outside its bar,
// since a click inside a portal still bubbles to the bar and folds it.
export function RestartOrchestratorButton({
  projectName,
  off,
  button,
  onAsk,
}: {
  projectName: string;
  off: string | null;
  button: RefObject<HTMLButtonElement | null>;
  onAsk: () => void;
}) {
  return (
    <button
      ref={button}
      type="button"
      className="btn sm restart-orch"
      disabled={off !== null}
      title={off ?? `Stop the ${projectName} orchestrator and start a new one`}
      onClick={(event: MouseEvent) => {
        event.stopPropagation();
        onAsk();
      }}
    >
      <ReopenIcon />
      Restart...
    </button>
  );
}

// The lane header's Start orchestrator. It sits in the header bar, so a
// click on it never folds the lane. A peer's project starts on that
// host (`host`); `off` says why the lane cannot act, and turns the button off.
export function StartOrchestratorButton({
  projectId,
  projectName,
  host,
  off,
}: {
  projectId: string;
  projectName: string;
  host?: string;
  off: string | null;
}) {
  const queryClient = useQueryClient();
  const [starting, setStarting] = useState(false);
  // The settings a refused start named. They are fixed in a dialog, since a
  // toast would name the first one only and offer no way to set it.
  const [fix, setFix] = useState<{ problems: SettingProblem[]; host: string | undefined } | null>(
    null,
  );
  // What a start that failed for another reason said, shown in the dialog.
  const [failure, setFailure] = useState<string | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const run = () => {
    setStarting(true);
    setFailure(null);
    void startOrchestrator(projectId, host)
      .then((result) => {
        if (!result.ok) {
          if (result.settings.length > 0) {
            setFix({ problems: result.settings, host: result.host });
          } else if (fix) {
            setFailure(result.message);
          } else {
            showToast("bad", "Could not start the orchestrator", result.message);
          }
          return;
        }
        setFix(null);
        const where = host ? ` on ${host}` : "";
        const title = result.replayed
          ? `The ${projectName} orchestrator is already running${where}`
          : `Started the ${projectName} orchestrator${where}`;
        showToast(
          result.warning ? "bad" : "ok",
          title,
          result.warning ??
            (host
              ? `It shows in the lane header once ${host} reports it.`
              : "It shows in the lane header once its pane starts."),
        );
        void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
      })
      .finally(() => setStarting(false));
  };
  const close = () => {
    setFix(null);
    setFailure(null);
    button.current?.focus();
  };
  return (
    <>
      <button
        ref={button}
        type="button"
        className="btn sm primary start-orch"
        disabled={starting || off !== null}
        title={off ?? `Start the ${projectName} orchestrator${host ? ` on ${host}` : ""}`}
        onClick={(event: MouseEvent) => {
          event.stopPropagation();
          run();
        }}
      >
        {starting ? "Starting..." : "Start orchestrator"}
      </button>
      {fix ? (
        // A click inside a portal still bubbles to the lane's bar, which
        // would fold the lane; it stops here.
        <span
          style={{ display: "contents" }}
          onClick={(event: MouseEvent) => event.stopPropagation()}
        >
          <StartSettingsDialog
            // A new list is a new form: what was saved before is not asked again.
            key={fix.problems.map((problem) => problem.key).join(" ")}
            projectId={projectId}
            projectName={projectName}
            problems={fix.problems}
            host={fix.host}
            failure={failure}
            starting={starting}
            onStart={run}
            onClose={close}
          />
        </span>
      ) : null}
    </>
  );
}

// What a start refused for its settings opens: every folder the start needs,
// each with a field and a Save (or, for a peer, the command to run there),
// and the start again.
function StartSettingsDialog({
  projectId,
  projectName,
  problems,
  host,
  failure,
  starting,
  onStart,
  onClose,
}: {
  projectId: string;
  projectName: string;
  problems: SettingProblem[];
  host: string | undefined;
  failure: string | null;
  starting: boolean;
  onStart: () => void;
  onClose: () => void;
}) {
  const roots = useQuery(startSettingsQuery()).data;
  return (
    <Modal
      className="dialog run-dlg start-fix-dlg"
      labelledBy="start-fix-title"
      describedBy="start-fix-body"
      dismissable={!starting}
      onClose={onClose}
    >
      <h2 id="start-fix-title">The {projectName} orchestrator cannot start yet</h2>
      <p id="start-fix-body">
        {host
          ? `${host} needs ${problems.length === 1 ? "a folder" : "folders"} that it does not have yet. Set ${problems.length === 1 ? "it" : "them"} on ${host}, then start again.`
          : `A start needs ${problems.length === 1 ? "a folder" : "folders"} that this host does not have yet. Save ${problems.length === 1 ? "it" : "them"} here, then start again.`}
      </p>
      <StartSettingsFix
        problems={problems}
        project={projectId}
        host={host}
        checkoutsRoot={roots?.checkouts_root ?? null}
        done="Saved. Press Start orchestrator."
      />
      {failure ? (
        <p className="refusal" role="alert">
          {failure}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button type="button" className="btn" disabled={starting} onClick={onClose}>
          Cancel
        </button>
        <button type="button" className="btn primary" disabled={starting} onClick={onStart}>
          {starting ? "Starting..." : "Start orchestrator"}
        </button>
      </div>
    </Modal>
  );
}
