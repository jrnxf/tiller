import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";

import type { AskRecorded, Project } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { agentsPath, queryKeyFor, threadPath } from "../../lib/queries";
import { isSentUncertain } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { startPath } from "../board/orchestrator-start";
import { showToast } from "../board/toast";

// One draft's idempotency keys. A retry of the same words reuses them, so a
// failed send that did land records one ask, never two. Other words are
// another draft, and a send that landed frees the box for the next one.
interface Draft {
  text: string;
  start: string;
  ask: string;
}

const draftOf = (text: string): Draft => ({ text, start: ulid(), ask: ulid() });

// What a failed send says in its toast.
function failureDetail(error: ActionError, host: string | undefined): string {
  if (isSentUncertain(error)) {
    return `${host ?? "The other host"} did not answer, so it may have arrived. Send it again: it is applied once.`;
  }
  return error.message;
}

// The chat's own send: the message goes to this project's orchestrator as an
// ask, as the Ask dialog sends it. With no orchestrator running, it starts
// one first. Unlike the dialog's sends, the same words can go twice in one
// chat, so the keys belong to the draft, not to the words.
//
// The send is optimistic: the message shows in the thread at once, as
// `pending`, until the thread read that holds it arrives. A failure takes it
// out again, says why in a toast, and hands the words back to the box.
export function useChatSend(project: Project, running: boolean) {
  const queryClient = useQueryClient();
  const draft = useRef<Draft>(draftOf(""));
  const [pending, setPending] = useState<string | null>(null);

  const send = useCallback(
    async (text: string): Promise<boolean> => {
      if (pending !== null || text.trim() === "") return false;
      if (draft.current.text !== text) draft.current = draftOf(text);
      const keys = draft.current;
      const host = hostField(project);
      setPending(text);
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
        if (!running) void queryClient.invalidateQueries({ queryKey: queryKeyFor(agentsPath()) });
        // The pending message stays until the thread that holds it is read,
        // so it never blinks out between the two.
        await queryClient
          .invalidateQueries({ queryKey: queryKeyFor(threadPath(project.id, ownerOf(project))) })
          .catch(() => {});
        return true;
      } catch (caught) {
        const error =
          caught instanceof ActionError
            ? caught
            : new ActionError(0, "internal", "Something went wrong, so this was not sent.");
        showToast("bad", "Could not send your message", failureDetail(error, ownerOf(project)));
        return false;
      } finally {
        setPending(null);
      }
    },
    [pending, project, queryClient, running],
  );

  return { pending, send };
}
