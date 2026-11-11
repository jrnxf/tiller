import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";
import type { MouseEvent, RefObject } from "react";

import type { MutationResponse } from "../../lib/api-types";
import { agentsPath, queryKeyFor } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { ReopenIcon } from "../attention/icons";
import { ulid } from "../attention/ulid";
import { useAction } from "../attention/use-action";
import { Modal } from "../ui/modal";
import { showToast } from "./toast";
import "./run-menu.css";

export const startPath = (projectId: string) => `/api/v1/projects/${projectId}/orchestrator`;

// What the daemon returns: the started or live orchestrator (protocol.md 7.4).
type Started = { warning?: string | null } | null;

type StartResult =
  | { ok: true; replayed: boolean; warning: string | null }
  | { ok: false; message: string };

// `POST /api/v1/projects/{id}/orchestrator`: the same start as `tiller
// orchestrator start`, forwarded to `host` for a peer's project. A live
// orchestrator comes back replayed, never a second one, so each click is a
// fresh key. A refusal is shown with the daemon's own words (a missing checkout
// names the setting, an unreachable herdr says so), except where the board can
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
      return { ok: false, message: "The daemon did not answer, so no orchestrator started." };
    }
    // This host's 403 is the cookie, unless its owner marker names another
    // system; that refusal, and a peer's own (it names the peer), is shown
    // in its words.
    if (error.status === 403 && error.host === null && error.code !== "owner_marker") {
      return {
        ok: false,
        message:
          "This browser is not signed in as the owner. Run `tiller board --login` for a login link.",
      };
    }
    return { ok: false, message: error.message };
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
        warning ?? "It shows in the lane header once herdr reports it.",
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
  const start = (event: MouseEvent) => {
    event.stopPropagation();
    setStarting(true);
    void startOrchestrator(projectId, host)
      .then((result) => {
        if (!result.ok) {
          showToast("bad", "Could not start the orchestrator", result.message);
          return;
        }
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
              : "It shows in the lane header once herdr reports it."),
        );
        void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
      })
      .finally(() => setStarting(false));
  };
  return (
    <button
      type="button"
      className="btn sm primary start-orch"
      disabled={starting || off !== null}
      title={off ?? `Start the ${projectName} orchestrator in herdr${host ? ` on ${host}` : ""}`}
      onClick={start}
    >
      {starting ? "Starting..." : "Start orchestrator"}
    </button>
  );
}
