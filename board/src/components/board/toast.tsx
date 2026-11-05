import { useEffect } from "react";

import { createStore, useStore } from "../../lib/store";
import { CheckIcon } from "../shell/icons";
import { WarnIcon } from "./icons";

interface ToastState {
  id: number;
  tone: "ok" | "bad";
  title: string;
  detail: string | null;
}

const toastStore = createStore<ToastState | null>(null);
let counter = 0;

export function showToast(tone: ToastState["tone"], title: string, detail: string | null = null) {
  counter += 1;
  toastStore.set({ id: counter, tone, title, detail });
}

function clearToast() {
  toastStore.set(null);
}

// The shell mounts one, so every screen has it. A status line that fades after a few seconds; a failure stays longer.
export function ToastHost() {
  const toast = useStore(toastStore);
  const id = toast?.id;
  const tone = toast?.tone;
  useEffect(() => {
    if (id === undefined) return;
    const timer = setTimeout(clearToast, tone === "bad" ? 8000 : 4000);
    return () => clearTimeout(timer);
  }, [id, tone]);

  return (
    <div
      className="toast"
      data-testid="toast"
      aria-live="polite"
      data-open={toast !== null}
      data-tone={toast?.tone}
    >
      {toast ? (
        <>
          {toast.tone === "ok" ? <CheckIcon /> : <WarnIcon />}
          <div>
            <div className="t1">{toast.title}</div>
            {toast.detail ? <div className="t2">{toast.detail}</div> : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
