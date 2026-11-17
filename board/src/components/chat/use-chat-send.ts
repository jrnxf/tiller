import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";

import type { AskRecorded, Project } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { agentsPath, queryKeyFor, threadPath } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { startPath } from "../board/orchestrator-start";

// One draft's idempotency keys. A retry of the same words reuses them, so a
// failed send that did land records one ask, never two. Other words are
// another draft, and a send that landed frees the box for the next one.
interface Draft {
  text: string;
  start: string;
  ask: string;
}

const draftOf = (text: string): Draft => ({ text, start: ulid(), ask: ulid() });

export type SendState =
  | { status: "idle" }
  | { status: "sending" }
  | { status: "failed"; error: ActionError };

// The chat's own send: the message goes to this project's orchestrator as an
// ask, as the Ask dialog sends it. With no orchestrator running, it starts
// one first. Unlike the dialog's sends, the same words can go twice in one
// chat, so the keys belong to the draft, not to the words.
export function useChatSend(project: Project, running: boolean) {
  const queryClient = useQueryClient();
  const draft = useRef<Draft>(draftOf(""));
  const inFlight = useRef(false);
  const [state, setState] = useState<SendState>({ status: "idle" });

  const send = useCallback(
    async (text: string): Promise<boolean> => {
      if (inFlight.current || text.trim() === "") return false;
      if (draft.current.text !== text) draft.current = draftOf(text);
      const keys = draft.current;
      const host = hostField(project);
      inFlight.current = true;
      setState({ status: "sending" });
      try {
        if (!running) {
          await sendMutation("POST", startPath(project.id), {
            idempotency_key: keys.start,
            ...host,
          });
        }
        await sendMutation<AskRecorded>("POST", `/api/v1/projects/${project.id}/asks`, {
          text,
          idempotency_key: keys.ask,
          ...host,
        });
        draft.current = draftOf("");
        setState({ status: "idle" });
        // The new message shows at once, before its change event arrives.
        void queryClient.invalidateQueries({
          queryKey: queryKeyFor(threadPath(project.id, ownerOf(project))),
        });
        if (!running) void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
        return true;
      } catch (caught) {
        setState({
          status: "failed",
          error:
            caught instanceof ActionError
              ? caught
              : new ActionError(0, "internal", "Something went wrong, so this was not sent."),
        });
        return false;
      } finally {
        inFlight.current = false;
      }
    },
    [project, queryClient, running],
  );

  return { state, send };
}
