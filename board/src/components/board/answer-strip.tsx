import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { ReactNode } from "react";

import type { PromptDialog, PromptRead } from "../../lib/api-types";
import { formatLisbon } from "../../lib/clock";
import { ownerOf } from "../../lib/peers";
import { agentsQuery, runQuery } from "../../lib/queries";
import { CheckIcon } from "../shell/icons";
import { Modal } from "../ui/modal";
import { Ago } from "./ago";
import { ShieldIcon, WarnIcon } from "./icons";
import { ulid } from "../attention/ulid";
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
  // `unsure`: a peer took the answer and did not confirm it (504).
  | { kind: "refused"; at: string; detail: string; unsure?: boolean };

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

// A forwarded answer the peer did not confirm in its proof wait (protocol.md
// 7.4, Agent verbs on a peer): it may have been typed.
function unconfirmed(host: string): string {
  return `${host} did not answer within 60 seconds, so the answer may or may not have been typed. Tap the same option again to retry: it carries the same key, so it is typed once.`;
}

// A read is tried again only when a later one could find what this one did
// not: no dialog drawn yet, no answer, or a server fault such as an
// unreachable peer (503) or a late one (504). A refusal (a 502 from a
// read-scope peer, a 4xx for a session that waits on nothing) stays one.
function tryAgain(failures: number, error: Error): boolean {
  if (failures >= 2) return false;
  const status = error instanceof PromptReadError ? error.status : null;
  return status === null || (status >= 500 && status !== 502);
}

// Forwarded answers the peer has not confirmed, by host and session: one in
// flight, or one a 503 or 504 left open. They outlive the strip, so a view
// that would hide it while its peer lags keeps it up, and the same tap after
// it carries the same key, so it is typed once. The agent leaving the wait
// drops it, so the next wait's taps mint new keys.
type OpenAnswer = { tap: string; key: string };
const openAnswers = new Map<string, OpenAnswer>();
const listeners = new Set<() => void>();

function openKey(host: string, sessionId: string): string {
  return `${host}\n${sessionId}`;
}

function setOpen(id: string, answer: OpenAnswer | null) {
  if (!answer && !openAnswers.has(id)) return;
  if (answer) openAnswers.set(id, answer);
  else openAnswers.delete(id);
  for (const listener of listeners) listener();
}

// Drops every open answer whose agent the roster shows out of its wait, or
// no longer lists, so a later wait never replays an old key. Mounted once in
// the shell, it holds with no strip on screen.
export function useDropEndedAnswers() {
  const agents = useQuery(agentsQuery()).data;
  useEffect(() => {
    if (!agents || openAnswers.size === 0) return;
    const waits = new Set<string>();
    for (const agent of agents.orchestrators.flatMap((row) => [row, ...row.workers])) {
      const host = ownerOf(agent);
      const waiting = agent.activity === "waiting_permission" || agent.activity === "waiting_input";
      if (host && waiting) waits.add(openKey(host, agent.session_id));
    }
    for (const id of openAnswers.keys()) if (!waits.has(id)) setOpen(id, null);
  }, [agents]);
}

// True while a peer's agent has an answer the peer has not confirmed.
export function useAnswerOpen(host: string | undefined, sessionId: string): boolean {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => (host ? openAnswers.has(openKey(host, sessionId)) : false),
  );
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
  host,
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
  // The peer that owns the agent: the read and the answer go to it.
  host?: string;
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
  // later observation of the same wait (the screen verdict seeing the dialog
  // after the hook did) keeps it; a later one after an answer went is the next wait.
  const [wait, setWait] = useState<string | null>(null);
  const answered = answer?.kind === "sent" && answer.at === wait && answer.seen !== observedAt;
  const next =
    waiting && observedAt !== null ? (wait === null || answered ? observedAt : wait) : null;
  if (next !== wait) setWait(next);
  useEffect(() => {
    if (host && !waiting) setOpen(openKey(host, sessionId), null);
  }, [host, sessionId, waiting]);
  const read = useQuery({
    queryKey: ["prompt", host ?? null, sessionId, next],
    queryFn: async ({ signal }) => {
      const result = await readPane<PromptRead>(
        promptPath(sessionId),
        signal,
        host ? { host } : {},
      );
      if (!result.ok) throw new PromptReadError(result.detail, result.status);
      if (host && result.body.reason === "not_waiting") setOpen(openKey(host, sessionId), null);
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
    const same = `${shown.screen}:${n}`;
    const id = host ? openKey(host, sessionId) : null;
    const before = id ? openAnswers.get(id) : undefined;
    const key = before?.tap === same ? before.key : ulid();
    if (id) setOpen(id, { tap: same, key });
    const sent = await readPane(`${promptPath(sessionId)}/answer`, undefined, {
      option: n,
      screen: shown.screen,
      ...(host ? { host, idempotency_key: key } : {}),
    });
    // A 503 never reached the peer: the answer stays open, under its key.
    if (id && (sent.ok || (sent.status !== 503 && sent.status !== 504))) setOpen(id, null);
    if (host && !sent.ok && sent.status === 504) {
      setAnswer({ kind: "refused", at, detail: unconfirmed(host), unsure: true });
      return;
    }
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
              <b>{current.unsure ? "Sent, not confirmed." : "Not sent."}</b> {current.detail}
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
      host={host}
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
  host,
  folder,
  onClose,
  onExit,
}: {
  agent: StripAgent | null;
  host: string | undefined;
  folder: string | null;
  onClose: () => void;
  onExit: () => void;
}) {
  const name = agent?.name ?? "the agent";
  const run = useQuery({ ...runQuery(agent?.runId ?? "", host), enabled: Boolean(agent?.runId) });
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
