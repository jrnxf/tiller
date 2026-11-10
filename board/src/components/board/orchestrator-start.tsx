import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { MouseEvent } from "react";

import { agentsPath, queryKeyFor } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { showToast } from "./toast";

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
    // This host's 403 is the cookie; a peer's own refusal (it names the
    // peer) is shown in its words.
    if (error.status === 403 && error.host === null) {
      return {
        ok: false,
        message:
          "This browser is not signed in as the owner. Run `tiller board --login` for a login link.",
      };
    }
    return { ok: false, message: error.message };
  }
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
