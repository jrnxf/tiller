import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import type { QueryClient } from "@tanstack/react-query";

import type { AskedRow, AskRecorded, BoardResponse, Project } from "../../lib/api-types";
import { ownerOf } from "../../lib/peers";
import { API_KEY_PREFIX } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { sendMutation } from "../attention/api";
import { SendIcon } from "../attention/icons";
import { useAction } from "../attention/use-action";

const BOARD_PATH = "/api/v1/board";

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

// What the box says once the ask is recorded. With no orchestrator running the
// ask is still on the board, and the next one is told about it when it starts.
function receiptLine(project: Project, recorded: AskRecorded | null): string | null {
  if (!recorded) return null;
  return recorded.warning === "no_orchestrator"
    ? `Recorded. No orchestrator is running for ${project.name}: it will pick this up when it starts.`
    : `Sent to the ${project.name} orchestrator.`;
}

// Type an ask for a project, as if in its orchestrator's pane: it is recorded
// as an ask and the orchestrator is woken. The reply comes back as cards and
// Inbox items. Its height is fixed so sending never moves the lane.
export function AskBox({ project }: { project: Project }) {
  const queryClient = useQueryClient();
  const [text, setText] = useState("");
  const [recorded, setRecorded] = useState<AskRecorded | null>(null);
  // The board host does not forward an ask (protocol.md section 7.4).
  const owner = ownerOf(project);
  const off = owner ? `${owner} owns this project: ask its orchestrator there` : null;
  const body = text.trim();

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<AskRecorded>("POST", `/api/v1/projects/${project.id}/asks`, {
        text: body,
        idempotency_key,
      }),
    [project.id, body],
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
      setRecorded(response.data);
      setText("");
    },
    [queryClient],
  );
  const action = useAction<AskRecorded>({
    binding: `${project.id}:${body}`,
    send,
    onRecorded,
  });
  const blocked = off !== null || action.pending;
  const submit = () => {
    if (body !== "" && !blocked) action.trigger();
  };
  const line = off ?? receiptLine(project, recorded);

  return (
    <form
      className="ask-box"
      aria-label={`Ask the ${project.name} orchestrator`}
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div className="ask-line">
        <textarea
          maxLength={8000}
          className="ask-in"
          rows={1}
          aria-label={`Ask the ${project.name} orchestrator`}
          placeholder={`Ask the ${project.name} orchestrator`}
          value={text}
          disabled={off !== null}
          readOnly={action.pending}
          title={off ?? undefined}
          onChange={(event) => {
            setText(event.target.value);
            setRecorded(null);
          }}
          onKeyDown={(event) => {
            // Enter sends; Shift+Enter adds a line.
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              submit();
            }
          }}
        />
        <button
          type="submit"
          className="btn"
          disabled={body === "" || blocked}
          title={off ?? undefined}
        >
          <SendIcon />
          {action.pending ? "Sending..." : "Send"}
        </button>
      </div>
      <div className="ask-note" role="status" aria-live="polite">
        {action.error ? null : line}
      </div>
      <div className="ask-fail">
        <ActionFailure error={action.error} host={owner} />
      </div>
    </form>
  );
}
