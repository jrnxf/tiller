import { useCallback, useEffect, useEffectEvent } from "react";

import type { AgentRow } from "../../lib/api-types";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { showToast } from "./toast";

export const FOCUS_TOOLTIP = "Show this agent's pane in a terminal on this host";

export const focusPath = (sessionId: string) => `/api/v1/agents/${sessionId}/focus`;

// The command that shows the agent's pane in a terminal, for a person to run
// by hand: what Open in terminal starts (protocol.md 7.4).
export function attachCommand(agent: Pick<AgentRow, "session_id">) {
  return `tiller attach ${agent.session_id}`;
}

// What the daemon did (protocol.md 7.4): switched a `tiller attach` client
// here to the pane, switched it and brought its terminal app to the front,
// started the app with `tiller attach` on the pane, found nothing here that
// can show it, or could not open the app.
type Opened = "focused" | "raised" | "launched" | "unseen" | "failed";

export type FocusData = {
  session_id?: string;
  opened?: Opened;
  // The terminal app from `[attach] terminal`, when one is configured.
  terminal?: string | null;
  // What went wrong, when `opened` is "failed" or "unseen".
  problem?: string;
} | null;

export type FocusResult = { ok: true; data: FocusData } | { ok: false; message: string };

// The words for what the daemon did, and for `unseen` the command to copy.
export type FocusOutcome = {
  tone: "ok" | "bad";
  title: string;
  detail: string;
  command: string | null;
};

export function focusOutcome(data: FocusData, sessionId: string): FocusOutcome {
  const opened = data?.opened ?? "focused";
  const app = data?.terminal ?? "The terminal app";
  const command = attachCommand({ session_id: data?.session_id ?? sessionId });
  switch (opened) {
    case "unseen":
      return {
        tone: "bad",
        title: "No terminal here shows the agent",
        detail: `${data?.problem ? `${data.problem}. ` : ""}Run this in a terminal on the daemon's host:`,
        command,
      };
    case "failed":
      return {
        tone: "bad",
        title: `${app} did not open`,
        detail: data?.problem ?? `${app} did not open on the daemon's host.`,
        command: null,
      };
    case "raised":
      return {
        tone: "ok",
        title: `Brought ${app} to the front`,
        detail: "Its tiller attach window shows the agent's pane.",
        command: null,
      };
    case "launched":
      return {
        tone: "ok",
        title: `Opened the agent in ${app}`,
        detail: "The new window runs tiller attach on the agent's pane.",
        command: null,
      };
    default:
      return {
        tone: "ok",
        title: "Switched the terminal to the agent",
        detail: "A tiller attach window on this host shows the agent's pane.",
        command: null,
      };
  }
}

// `POST /api/v1/agents/{session}/focus`. Focus is not a state change, so each
// click is a fresh key: opening the same agent again later is a new open. A
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
    return { ok: false, message: focusRefusal(error) };
  }
}

// The words for a focus the daemon refused, or did not answer.
function focusRefusal(error: ActionError): string {
  if (error.status === 0) return "The daemon did not answer, so nothing was opened.";
  if (error.status === 503) {
    return `The terminal holder did not switch to the pane, so nothing was opened: ${error.message}`;
  }
  if (error.status === 403) {
    return "This browser is not signed in as the owner. Run `tiller board --login` for a login link.";
  }
  return error.message;
}

// Returns the click handler for "Open in terminal" (and the O key).
export function useOpenInTerminal() {
  return useCallback((agent: Pick<AgentRow, "session_id">) => {
    void focusAgent(agent.session_id).then((result) => {
      if (result.ok) {
        const outcome = focusOutcome(result.data, agent.session_id);
        showToast(outcome.tone, outcome.title, outcome.detail, outcome.command);
      } else {
        showToast("bad", "Could not open the terminal", result.message);
      }
    });
  }, []);
}

// The O key opens the selected agent in a terminal; a key typed into a field,
// or pressed while a dialog is open, never does. The listener stays put while
// `onOpen` changes from render to render.
export function useOpenKey(onOpen: (() => void) | null) {
  const on = onOpen !== null;
  const open = useEffectEvent(() => onOpen?.());
  useEffect(() => {
    if (!on) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== "o") return;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      if (shortcutsBlocked(event)) return;
      event.preventDefault();
      open();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [on]);
}
