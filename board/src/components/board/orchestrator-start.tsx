import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { KeyboardEvent, MouseEvent } from "react";

import { agentsPath, queryKeyFor } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { showToast } from "./toast";

const startPath = (projectId: string) => `/api/v1/projects/${projectId}/orchestrator`;

// What the daemon returns: the started or live orchestrator (protocol.md 7.4).
type Started = { warning?: string | null } | null;

type StartResult =
  | { ok: true; replayed: boolean; warning: string | null }
  | { ok: false; message: string };

// `POST /api/v1/projects/{id}/orchestrator`: the same start as `tiller
// orchestrator start`. A live orchestrator comes back replayed, never a second
// one, so each click is a fresh key. A refusal is shown with the daemon's own
// words (a missing checkout names the setting, an unreachable herdr says so),
// except where the board can say more about what to do.
async function startOrchestrator(projectId: string): Promise<StartResult> {
  try {
    const response = await sendMutation<Started>("POST", startPath(projectId), {
      idempotency_key: ulid(),
    });
    return { ok: true, replayed: response.replayed, warning: response.data?.warning ?? null };
  } catch (error) {
    if (!(error instanceof ActionError)) throw error;
    if (error.status === 0) {
      return { ok: false, message: "The daemon did not answer, so no orchestrator started." };
    }
    if (error.status === 403) {
      return {
        ok: false,
        message:
          "This browser is not signed in as the owner. Run `tiller board --login` for a login link.",
      };
    }
    return { ok: false, message: error.message };
  }
}

// The lane header's Start orchestrator. It sits inside the header's toggle, so
// a click or key on it never folds the lane.
export function StartOrchestratorButton({
  projectId,
  projectName,
}: {
  projectId: string;
  projectName: string;
}) {
  const queryClient = useQueryClient();
  const [starting, setStarting] = useState(false);
  const start = (event: MouseEvent) => {
    event.stopPropagation();
    setStarting(true);
    void startOrchestrator(projectId)
      .then((result) => {
        if (!result.ok) {
          showToast("bad", "Could not start the orchestrator", result.message);
          return;
        }
        const title = result.replayed
          ? `The ${projectName} orchestrator is already running`
          : `Started the ${projectName} orchestrator`;
        showToast(
          result.warning ? "bad" : "ok",
          title,
          result.warning ?? "It shows in the lane header once herdr reports it.",
        );
        void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
      })
      .finally(() => setStarting(false));
  };
  return (
    <button
      type="button"
      className="btn sm primary start-orch"
      disabled={starting}
      title={`Start the ${projectName} orchestrator in herdr`}
      onClick={start}
      onKeyDown={(event: KeyboardEvent) => event.stopPropagation()}
    >
      {starting ? "Starting..." : "Start orchestrator"}
    </button>
  );
}
