import { useSuspenseQuery } from "@tanstack/react-query";
import { useCallback, useEffect } from "react";

import type { AgentRow } from "../../lib/api-types";
import { machinesQuery } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { showToast } from "./toast";

const FOCUS_TOOLTIP = "Focus this agent's terminal in herdr";
const FOCUS_TOOLTIP_REMOTE =
  "Opens herdr on the remote machine first, then focuses this agent's terminal";

export const focusPath = (sessionId: string) => `/api/v1/agents/${sessionId}/focus`;

// The command the daemon runs for the jump, shown so a person can run it by
// hand: `<card>-<role>` is the agent name herdr knows (herdr-layout.md).
export function focusCommand(
  agent: Pick<AgentRow, "herdr_name" | "role">,
  projectId: string,
  cardDisplayId: string | null = null,
) {
  const owner = agent.role === "orchestrator" ? projectId : (cardDisplayId ?? projectId);
  const name =
    agent.herdr_name ?? `${owner}-${agent.role === "orchestrator" ? "orch" : agent.role}`;
  return `herdr agent focus ${name}`;
}

export type FocusResult = { ok: true } | { ok: false; message: string };

// `POST /api/v1/agents/{session}/focus`. Focus is not a state change, so each
// click is a fresh key: focusing the same agent again later is a new jump. A
// refusal is shown with the daemon's own words, except where the board can say
// more about what to do.
export async function focusAgent(sessionId: string): Promise<FocusResult> {
  try {
    await sendMutation("POST", focusPath(sessionId), { idempotency_key: ulid() });
    return { ok: true };
  } catch (error) {
    if (!(error instanceof ActionError)) throw error;
    if (error.status === 0 || error.status === 503) {
      return { ok: false, message: "herdr did not answer, so nothing was focused." };
    }
    if (error.status === 403) {
      return {
        ok: false,
        message: "This browser is not signed in as the owner. Run `tiller board` for a login link.",
      };
    }
    return { ok: false, message: error.message };
  }
}

// Returns the click handler for "Open in herdr" (and the O key).
export function useHerdrFocus() {
  const { data: machines } = useSuspenseQuery(machinesQuery());
  return useCallback(
    (agent: Pick<AgentRow, "session_id" | "host_id">) => {
      const machine = machines.machines.find((candidate) => candidate.id === agent.host_id);
      const remote = machine?.kind === "remote";
      void focusAgent(agent.session_id).then((result) => {
        if (result.ok) {
          showToast(
            "ok",
            remote
              ? `Opened herdr on ${machine?.label ?? "the remote machine"}`
              : "Focused in herdr",
            "The agent's terminal is in front.",
          );
        } else {
          showToast("bad", "Could not open herdr", result.message);
        }
      });
    },
    [machines],
  );
}

export function focusTooltip(host: { kind: "local" | "remote" } | undefined): string {
  return host?.kind === "remote" ? FOCUS_TOOLTIP_REMOTE : FOCUS_TOOLTIP;
}

function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName);
}

// The O key opens the selected agent in herdr; a key typed into a field never
// does.
export function useOpenKey(onOpen: (() => void) | null) {
  useEffect(() => {
    if (!onOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== "o") return;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      if (isTyping(event.target)) return;
      event.preventDefault();
      onOpen();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onOpen]);
}
