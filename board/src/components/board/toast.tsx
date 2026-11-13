import { useEffect, useState } from "react";

import { createStore, useStore } from "../../lib/store";
import { CheckIcon } from "../shell/icons";
import { WarnIcon } from "./icons";

interface ToastState {
  id: number;
  tone: "ok" | "bad";
  title: string;
  detail: string | null;
  // A command for the owner to run, shown with a Copy button.
  command: string | null;
}

const toastStore = createStore<ToastState | null>(null);
let counter = 0;

export function showToast(
  tone: ToastState["tone"],
  title: string,
  detail: string | null = null,
  command: string | null = null,
) {
  counter += 1;
  toastStore.set({ id: counter, tone, title, detail, command });
}

// How long a toast stays: a failure longer, and one with a command to copy
// long enough to copy it. A pointer on the toast holds it.
const OK_MS = 4000;
const BAD_MS = 8000;
const COMMAND_MS = 20_000;

// A command with its Copy button: the toast's, and any detail's.
export function CopyCommand({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const copy = () => {
    navigator.clipboard?.writeText(command).then(
      () => setCopied(true),
      () => undefined,
    );
  };
  return (
    <span className="copy-cmd">
      <code>{command}</code>
      <button type="button" className="btn sm" onClick={copy} aria-label={`Copy ${command}`}>
        {copied ? "Copied" : "Copy"}
      </button>
    </span>
  );
}

function clearToast() {
  toastStore.set(null);
}

// The shell mounts one, so every screen has it. A status line that fades after a few seconds; a failure stays longer.
export function ToastHost() {
  const toast = useStore(toastStore);
  const id = toast?.id;
  const tone = toast?.tone;
  const command = toast?.command ?? null;
  const [held, setHeld] = useState(false);
  useEffect(() => {
    if (id === undefined || held) return;
    const timer = setTimeout(
      clearToast,
      command !== null ? COMMAND_MS : tone === "bad" ? BAD_MS : OK_MS,
    );
    return () => clearTimeout(timer);
  }, [id, tone, command, held]);

  return (
    <div
      className="toast"
      data-testid="toast"
      aria-live="polite"
      // An open dialog leaves it live, so a toast is still announced.
      data-modal-exempt=""
      data-open={toast !== null}
      data-tone={toast?.tone}
      data-command={command !== null}
      onPointerEnter={() => setHeld(true)}
      onPointerLeave={() => setHeld(false)}
    >
      {toast ? (
        <>
          {toast.tone === "ok" ? <CheckIcon /> : <WarnIcon />}
          <div>
            <div className="t1">{toast.title}</div>
            {toast.detail ? <div className="t2">{toast.detail}</div> : null}
            {toast.command ? (
              <div className="t3">
                <CopyCommand command={toast.command} />
              </div>
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
