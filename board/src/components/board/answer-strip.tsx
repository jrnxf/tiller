import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { ReactNode } from "react";

import type { PromptDialog, PromptRead } from "../../lib/api-types";
import { CheckIcon } from "../shell/icons";
import { Ago } from "./ago";
import { ShieldIcon, WarnIcon } from "./icons";
import { promptPath, readPane } from "./terminal-stream";
import "./answer-strip.css";

// How long the receipt stays before the strip folds away.
const RECEIPT_MS = 1800;

const CHANGED =
  "The prompt changed after these buttons were drawn, so nothing was typed. These are the new prompt's options; tap again if you still want one.";

// What the last tap did, in the wait it was made in (`at`): a new wait
// starts over. A sent answer keeps the activity time it was sent at (`seen`).
type Answer =
  | { kind: "sending"; at: string; n: number }
  | { kind: "sent"; at: string; seen: string; n: number; text: string; folded: boolean }
  | { kind: "refused"; at: string; detail: string };

// The answer buttons under a waiting agent's terminal (protocol.md 7.4,
// Prompt). The screen is read once each time the agent's hook says it waits
// on a permission prompt, never polled, though a read that finds no dialog is
// tried twice more, as the hook can beat the dialog's draw. A tap carries the
// dialog it was drawn from, and the daemon types nothing if the screen no
// longer shows it.
export function AnswerStrip({
  sessionId,
  waiting,
  observedAt,
  fallback = null,
}: {
  sessionId: string;
  waiting: boolean;
  observedAt: string;
  // What shows in the strip's place once a read found no dialog to answer.
  fallback?: ReactNode;
}) {
  const [answer, setAnswer] = useState<Answer | null>(null);
  // The wait the strip answers, named by the activity time it began at. A
  // later observation of the same wait (herdr seeing the dialog after the
  // hook did) keeps it; a later one after an answer went is the next wait.
  const [wait, setWait] = useState<string | null>(null);
  const answered = answer?.kind === "sent" && answer.at === wait && answer.seen !== observedAt;
  const next = waiting ? (wait === null || answered ? observedAt : wait) : null;
  if (next !== wait) setWait(next);
  const read = useQuery({
    queryKey: ["prompt", sessionId, next],
    queryFn: async ({ signal }) => {
      const result = await readPane<PromptRead>(promptPath(sessionId), signal);
      if (!result.ok) throw new Error(result.detail);
      if (result.body.reason === "no_dialog") throw new Error("no dialog on the screen yet");
      return result.body;
    },
    enabled: next !== null,
    staleTime: Infinity,
    gcTime: 0,
    retry: 2,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  // The receipt outlasts the wait: the agent moves on as soon as it reads the key.
  const current = answer && (answer.at === next || !waiting) ? answer : null;
  // A read again after a refused tap that finds no dialog draws none.
  const dialog = waiting && !read.isError ? read.data?.prompt : null;

  const tap = async (shown: PromptDialog, n: number, text: string) => {
    const at = wait ?? observedAt;
    const seen = observedAt;
    setAnswer({ kind: "sending", at, n });
    const sent = await readPane(`${promptPath(sessionId)}/answer`, undefined, {
      option: n,
      screen: shown.screen,
    });
    if (sent.ok) {
      setAnswer({ kind: "sent", at, seen, n, text, folded: false });
      setTimeout(() => {
        setAnswer((now) =>
          now?.kind === "sent" && now.at === at ? { ...now, folded: true } : now,
        );
      }, RECEIPT_MS);
      return;
    }
    if (sent.status === 409) await read.refetch();
    setAnswer({ kind: "refused", at, detail: sent.status === 409 ? CHANGED : sent.detail });
  };

  let strip = null;
  if (current?.kind === "sent") {
    strip = (
      <div className="av-strip done">
        <div className="av-sent" role="status">
          <CheckIcon />
          <span>
            Sent <code>{current.n}</code> · {current.text}.
          </span>
        </div>
      </div>
    );
  } else if (dialog && read.data) {
    const busy = current?.kind === "sending" ? current.n : null;
    const fresh = current?.kind === "refused" ? " av-fresh-t" : "";
    strip = (
      <div className="av-strip">
        {current?.kind === "refused" ? (
          <div className="av-refused" role="alert">
            <WarnIcon />
            <div>
              <b>Not sent.</b> {current.detail}
            </div>
          </div>
        ) : null}
        <div className="av-strip-h">
          <span className="q">
            <i className="ld waiting" />
            {dialog.command === null ? dialog.question : `${dialog.tool} wants to run`}
          </span>
          {dialog.command === null ? null : (
            <span className={`cmd${fresh}`} title={dialog.command}>
              {dialog.command}
            </span>
          )}
          <span className="src">
            <ShieldIcon />
            <span>
              <Ago at={read.data.read_at} prefix="Read from the screen" />
              {` · matches the hook (${dialog.tool})`}
            </span>
          </span>
        </div>
        <div className="av-opts">
          {dialog.options.map(({ n, text }) => (
            <button
              key={n}
              type="button"
              className={`av-ans${n === 1 ? " first" : ""}${busy !== null && busy !== n ? " dim" : ""}`}
              disabled={busy !== null}
              data-n={n}
              aria-label={`Answer ${n}: ${text}`}
              onClick={() => void tap(dialog, n, text)}
            >
              {busy === n ? <span className="av-ans-spin" /> : <kbd>{n}</kbd>}
              <span className={`t${fresh}`}>{text}</span>
            </button>
          ))}
        </div>
      </div>
    );
  }
  const open = strip !== null && !(current?.kind === "sent" && current.folded);
  if (strip === null && read.fetchStatus === "idle" && fallback !== null) return fallback;
  return (
    <div className={`av-strip-w${open ? "" : " shut"}`}>
      <div>{strip}</div>
    </div>
  );
}
