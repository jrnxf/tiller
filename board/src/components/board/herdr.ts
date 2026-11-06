import { useSuspenseQuery } from "@tanstack/react-query";
import { useCallback, useEffect } from "react";

import type { AgentRow, Machine } from "../../lib/api-types";
import { machinesQuery } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { showToast } from "./toast";

const FOCUS_TOOLTIP = "Focus this agent's terminal in herdr";
const FOCUS_TOOLTIP_REMOTE =
  "Focuses this agent's terminal on the remote machine; select that machine in herdr's sidebar to see it";

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

// What the daemon did (protocol.md 7.4): focused the pane only, brought the
// terminal app with herdr in it to the front, opened herdr in the app,
// focused a pane no herdr window on its machine shows, or focused the pane
// but could not bring a window up.
type Opened = "focused" | "raised" | "launched" | "unseen" | "failed";

export type FocusData = {
  session_id?: string;
  opened?: Opened;
  // The terminal app from `[herdr] terminal`, when one is configured.
  terminal?: string | null;
  // What went wrong with the window, when `opened` is "failed".
  problem?: string;
} | null;

export type FocusResult = { ok: true; data: FocusData } | { ok: false; message: string };

export type FocusOutcome = { tone: "ok" | "bad"; title: string; detail: string };

// The words for what the jump did. A daemon that does not say is an older one,
// which only focused. herdr's window shows one server at a time, so a remote
// agent's pane appears once that machine is selected in its sidebar.
export function focusOutcome(data: FocusData, remote: string | null = null): FocusOutcome {
  const opened = data?.opened ?? "focused";
  const app = data?.terminal ?? "the terminal";
  if (opened === "unseen") {
    return {
      tone: "bad",
      title: "No herdr window here",
      detail:
        "herdr focused the agent, but no herdr window is open on the daemon's machine. Start herdr there, or open the board on the machine you are at.",
    };
  }
  if (opened === "failed") {
    return {
      tone: "bad",
      title: "herdr window did not open",
      detail: `herdr focused the agent, but ${data?.problem ?? `${app} did not open`}.`,
    };
  }
  const where = remote ? `Select ${remote} in herdr's sidebar to see the agent's terminal.` : null;
  switch (opened) {
    case "raised":
      return {
        tone: "ok",
        title: "Brought herdr to the front",
        detail: where ?? `herdr is in front in ${app}, on the agent's terminal.`,
      };
    case "launched":
      return {
        tone: "ok",
        title: `Opened herdr in ${app}`,
        detail: where ?? "The new herdr window is on the agent's terminal.",
      };
    default:
      return {
        tone: "ok",
        title: remote ? `Focused on ${remote}` : "Focused in herdr",
        detail: where ?? "herdr shows the agent's terminal.",
      };
  }
}

// `POST /api/v1/agents/{session}/focus`. Focus is not a state change, so each
// click is a fresh key: focusing the same agent again later is a new jump. A
// refusal is shown with the daemon's own words, except where the board can say
// more about what to do.
export async function focusAgent(sessionId: string): Promise<FocusResult> {
  try {
    const response = await sendMutation<FocusData>("POST", focusPath(sessionId), {
      idempotency_key: ulid(),
    });
    return { ok: true, data: response.data };
  } catch (error) {
    if (!(error instanceof ActionError)) throw error;
    if (error.status === 0 || error.status === 503) {
      return { ok: false, message: "herdr did not answer, so nothing was focused." };
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

// The label of the machine a host id names when it is a remote one, and null
// for this machine.
export function remoteLabel(machines: Machine[], hostId: string | null): string | null {
  const machine = machines.find((candidate) => candidate.id === hostId);
  return machine?.kind === "remote" ? machine.label : null;
}

// Returns the click handler for "Open in herdr" (and the O key).
export function useHerdrFocus() {
  const { data: machines } = useSuspenseQuery(machinesQuery());
  return useCallback(
    (agent: Pick<AgentRow, "session_id" | "host_id">) => {
      const remote = remoteLabel(machines.machines, agent.host_id);
      void focusAgent(agent.session_id).then((result) => {
        if (result.ok) {
          const outcome = focusOutcome(result.data, remote);
          showToast(outcome.tone, outcome.title, outcome.detail);
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

export function isTyping(target: EventTarget | null): boolean {
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
