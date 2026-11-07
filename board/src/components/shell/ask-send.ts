import { useQueryClient } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";

import type { AskedRow, AskRecorded, BoardResponse, Project } from "../../lib/api-types";
import { hostField, projectKey } from "../../lib/peers";
import { API_KEY_PREFIX } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";

const BOARD_PATH = "/api/v1/board";

// One project an ask goes to.
export interface AskTarget {
  project: Project;
  // Whether its orchestrator is running now; when not, the send starts one.
  running: boolean;
  // The words this project receives.
  text: string;
}

// How one project's send ended, or is going.
export type Outcome =
  | { status: "sending" }
  | { status: "sent"; started: boolean }
  | { status: "failed"; error: ActionError };

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

function asError(caught: unknown): ActionError {
  return caught instanceof ActionError
    ? caught
    : new ActionError(0, "internal", "Something went wrong, so this was not saved.");
}

// The sends of one Ask dialog: one ask per project, each on the project's own
// path (a peer's through the board host). A project with no orchestrator running
// gets one started first. Every send keeps its idempotency keys for as long as
// the words are the same, so retrying a failed one never records a second ask.
// Which projects failed and what the dialog then offers is the dialog's to say;
// this only reports each project's outcome.
export function useAskSends() {
  const queryClient = useQueryClient();
  const [outcomes, setOutcomes] = useState<Record<string, Outcome>>({});
  const keys = useRef(new Map<string, string>());

  const keyFor = useCallback((target: AskTarget, op: "start" | "ask") => {
    const slot = `${projectKey(target.project)}|${op}|${target.text}`;
    let key = keys.current.get(slot);
    if (!key) {
      key = ulid();
      keys.current.set(slot, key);
    }
    return key;
  }, []);

  const sendOne = useCallback(
    async (target: AskTarget): Promise<Outcome> => {
      const { project } = target;
      const host = hostField(project);
      try {
        if (!target.running) {
          await sendMutation("POST", `/api/v1/projects/${project.id}/orchestrator`, {
            idempotency_key: keyFor(target, "start"),
            ...host,
          });
        }
        const response = await sendMutation<AskRecorded>(
          "POST",
          `/api/v1/projects/${project.id}/asks`,
          { text: target.text, idempotency_key: keyFor(target, "ask"), ...host },
        );
        // A peer's ask comes to the board in the change event instead.
        if (!host.host) {
          const { ask } = response.data;
          showAsk(queryClient, {
            id: ask.id,
            project_id: ask.project_id,
            session_id: ask.session_id,
            received_at: ask.received_at,
            age_seconds: 0,
          });
        }
        return { status: "sent", started: !target.running };
      } catch (caught) {
        return { status: "failed", error: asError(caught) };
      }
    },
    [keyFor, queryClient],
  );

  // Sends to these projects together; each outcome shows as it lands.
  const send = useCallback(
    async (targets: AskTarget[]): Promise<Record<string, Outcome>> => {
      setOutcomes((now) => {
        const next = { ...now };
        for (const target of targets) next[projectKey(target.project)] = { status: "sending" };
        return next;
      });
      const landed: Record<string, Outcome> = {};
      await Promise.all(
        targets.map(async (target) => {
          const outcome = await sendOne(target);
          landed[projectKey(target.project)] = outcome;
          setOutcomes((now) => ({ ...now, [projectKey(target.project)]: outcome }));
        }),
      );
      return landed;
    },
    [sendOne],
  );

  return { outcomes, send };
}
