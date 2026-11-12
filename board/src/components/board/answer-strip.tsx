import { useQuery } from "@tanstack/react-query";
import { useRef, useState } from "react";
import type { ReactNode } from "react";

import type { PromptDialog, PromptRead } from "../../lib/api-types";
import { formatLisbon } from "../../lib/clock";
import { runQuery } from "../../lib/queries";
import { CheckIcon } from "../shell/icons";
import { Modal } from "../ui/modal";
import { Ago } from "./ago";
import { ShieldIcon, WarnIcon } from "./icons";
import { promptPath, readPane } from "./terminal-stream";
import "./answer-strip.css";

// How long the receipt stays before the strip folds away.
const RECEIPT_MS = 1800;

const CHANGED =
  "The prompt changed after these buttons were drawn, so nothing was typed. These are the new prompt's options; tap again if you still want one.";

// The trust dialog's option that ends the session: it asks first.
const TRUST_EXIT = "No, exit";

// What the last tap did, in the wait it was made in (`at`): a new wait
// starts over. A sent answer keeps the activity time it was sent at (`seen`).
type Answer =
  | { kind: "sending"; at: string; n: number }
  | {
      kind: "sent";
      at: string;
      seen: string | null;
      n: number;
      text: string;
      // A cursor menu takes arrow keys, not the digit, so the receipt names no key.
      digit: boolean;
      folded: boolean;
    }
  | { kind: "refused"; at: string; detail: string };

// A prompt read that failed: `status` is the daemon's, or null when it did
// not answer or found no dialog yet.
class PromptReadError extends Error {
  constructor(
    message: string,
    readonly status: number | null,
  ) {
    super(message);
  }
}

// A read is tried again only when a later one could find what this one did
// not: no dialog drawn yet, no answer, or a server fault. A refusal (a saved
// machine's agent, a session that waits on nothing) stays one.
function tryAgain(failures: number, error: Error): boolean {
  if (failures >= 2) return false;
  const status = error instanceof PromptReadError ? error.status : null;
  return status === null || status >= 500;
}

// The waiting agent, for the question "No, exit" asks first: its name, and
// its card and run for the facts.
export interface StripAgent {
  name: string;
  card: string | null;
  runId: string | null;
}

// The answer buttons under a waiting agent's terminal (protocol.md 7.4,
// Prompt). The screen is read once each time the agent's session says it
// waits, on a permission prompt or on an answer, never polled, though a read
// that finds no dialog is tried twice more, as the hook can beat the dialog's
// draw (a refused read is not). A tap carries the dialog it was drawn from, and the daemon types
// nothing if the screen no longer shows it. A numbered dialog's buttons show
// their digits; a cursor menu's show none, as its rows have none.
export function AnswerStrip({
  sessionId,
  waiting,
  observedAt,
  agent = null,
  fallback = null,
  reading = null,
  lead,
  onSent,
}: {
  sessionId: string;
  waiting: boolean;
  // Null while the daemon has not read the pane: there is no wait to answer.
  observedAt: string | null;
  agent?: StripAgent | null;
  // What shows in the strip's place once a read found no dialog to answer.
  fallback?: ReactNode;
  // What shows while the first read is out.
  reading?: ReactNode;
  // What shows above the strip for the dialog it draws.
  lead?: (dialog: PromptDialog) => ReactNode;
  // Told the receipt's wording once an answer was sent.
  onSent?: (receipt: string) => void;
}) {
  const [answer, setAnswer] = useState<Answer | null>(null);
  // The option whose answer waits on the owner's yes: "No, exit" ends the session.
  const [confirm, setConfirm] = useState<{ dialog: PromptDialog; n: number } | null>(null);
  // The wait the strip answers, named by the activity time it began at. A
  // later observation of the same wait (herdr seeing the dialog after the
  // hook did) keeps it; a later one after an answer went is the next wait.
  const [wait, setWait] = useState<string | null>(null);
  const answered = answer?.kind === "sent" && answer.at === wait && answer.seen !== observedAt;
  const next =
    waiting && observedAt !== null ? (wait === null || answered ? observedAt : wait) : null;
  if (next !== wait) setWait(next);
  const read = useQuery({
    queryKey: ["prompt", sessionId, next],
    queryFn: async ({ signal }) => {
      const result = await readPane<PromptRead>(promptPath(sessionId), signal);
      if (!result.ok) throw new PromptReadError(result.detail, result.status);
      if (result.body.reason === "no_dialog") {
        throw new PromptReadError("no dialog on the screen yet", null);
      }
      return result.body;
    },
    enabled: next !== null,
    staleTime: Infinity,
    gcTime: 0,
    retry: tryAgain,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  // The receipt outlasts the wait: the agent moves on as soon as it reads the key.
  const current = answer && (answer.at === next || !waiting) ? answer : null;
  // A read again after a refused tap that finds no dialog draws none.
  const dialog = waiting && !read.isError ? read.data?.prompt : null;

  const tap = async (shown: PromptDialog, n: number, text: string) => {
    // A dialog shows only for a wait the strip has named.
    if (wait === null) return;
    const at = wait;
    const seen = observedAt;
    const digit = shown.layout === "permission";
    setAnswer({ kind: "sending", at, n });
    const sent = await readPane(`${promptPath(sessionId)}/answer`, undefined, {
      option: n,
      screen: shown.screen,
    });
    if (sent.ok) {
      setAnswer({ kind: "sent", at, seen, n, text, digit, folded: false });
      onSent?.(digit ? `${n} · ${text}` : text);
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
            Sent{" "}
            {current.digit ? (
              <>
                <code>{current.n}</code> ·{" "}
              </>
            ) : null}
            {current.text}.
          </span>
        </div>
      </div>
    );
  } else if (dialog && read.data) {
    const busy = current?.kind === "sending" ? current.n : null;
    const fresh = current?.kind === "refused" ? " av-fresh-t" : "";
    const trust = dialog.layout === "trust";
    const subject = trust ? dialog.folder : dialog.command;
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
            {trust
              ? "Claude Code asks to trust"
              : dialog.command === null
                ? dialog.question
                : `${dialog.tool} wants to run`}
          </span>
          {subject === null ? null : (
            <span className={`cmd${fresh}`} title={subject}>
              {subject}
            </span>
          )}
          <span className="src">
            <ShieldIcon />
            <span>
              <Ago at={read.data.read_at} prefix="Read from the screen" />
              {trust ? " · known layout: trust" : ` · matches the hook (${dialog.tool})`}
            </span>
          </span>
        </div>
        {trust ? (
          // A cursor menu: no digits, the rows in screen order, and the
          // answer that keeps the agent working filled on the right.
          <div className="av-opts end">
            {dialog.options.map(({ n, text }) => {
              const exit = text === TRUST_EXIT;
              return (
                <button
                  key={n}
                  type="button"
                  className={`av-ans bare${exit ? "" : " first"}${busy !== null && busy !== n ? " dim" : ""}`}
                  disabled={busy !== null}
                  aria-haspopup={exit ? "dialog" : undefined}
                  onClick={() => (exit ? setConfirm({ dialog, n }) : void tap(dialog, n, text))}
                >
                  {busy === n ? <span className="av-ans-spin" /> : null}
                  <span className={`t${fresh}`}>{exit ? `${text}...` : text}</span>
                </button>
              );
            })}
          </div>
        ) : (
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
        )}
      </div>
    );
  }
  const exitDialog = confirm ? (
    <ExitDialog
      agent={agent}
      folder={confirm.dialog.folder}
      onClose={() => setConfirm(null)}
      onExit={() => {
        const { dialog: shown, n } = confirm;
        setConfirm(null);
        void tap(shown, n, TRUST_EXIT);
      }}
    />
  ) : null;
  const open = strip !== null && !(current?.kind === "sent" && current.folded);
  if (strip === null && read.fetchStatus === "idle" && fallback !== null) return fallback;
  if (strip === null && read.fetchStatus !== "idle" && reading !== null) return reading;
  const shown = dialog ?? (current?.kind === "sent" ? read.data?.prompt : null);
  return (
    <>
      {shown && lead ? lead(shown) : null}
      <div className={`av-strip-w${open ? "" : " shut"}`}>
        <div>{strip}</div>
      </div>
      {exitDialog}
    </>
  );
}

// "No, exit" ends the agent's session, so it asks first.
function ExitDialog({
  agent,
  folder,
  onClose,
  onExit,
}: {
  agent: StripAgent | null;
  folder: string | null;
  onClose: () => void;
  onExit: () => void;
}) {
  const name = agent?.name ?? "the agent";
  const run = useQuery({ ...runQuery(agent?.runId ?? ""), enabled: Boolean(agent?.runId) });
  const started = formatLisbon(run.data?.started_at ?? null);
  const card = agent?.card ?? null;
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <Modal
      className="dialog av-exit"
      labelledBy="exit-dialog-title"
      describedBy="exit-dialog-body"
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="exit-dialog-title">Exit {name}'s session?</h2>
      <p id="exit-dialog-body">
        Claude Code exits, so {name} stops and its run ends. The card stays where it is.
      </p>
      <dl className="av-exit-facts">
        {folder === null ? null : (
          <>
            <dt>Folder</dt>
            <dd className="mono">{folder}</dd>
          </>
        )}
        {card === null ? null : (
          <>
            <dt>Run</dt>
            <dd>{started ? `${card}, started ${started}` : card}</dd>
          </>
        )}
      </dl>
      <div className="dialog-actions">
        <button ref={keep} type="button" className="btn" onClick={onClose}>
          Keep waiting
        </button>
        <button type="button" className="btn danger" onClick={onExit}>
          Exit the session
        </button>
      </div>
    </Modal>
  );
}
