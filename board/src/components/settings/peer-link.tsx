import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";

import type { AcceptedKey, LinkState, Machine, PeerList, PeerScope } from "../../lib/api-types";
import { formatLisbon, formatLisbonDay } from "../../lib/clock";
import { machinesPath, peersPath, peersQuery, queryKeyFor } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { KeyIcon, UnlinkIcon } from "../shell/icons";
import { Modal } from "../ui/modal";
import { HostCardSection } from "./host-card";
import "../attention/inbox.css";
import "../shell/project-stage.css";
import "./settings.css";

// Each side of a link cuts the end it owns (protocol.md section 10.1). This
// board unpairs a host it added, as `tiller peer remove` does; this host
// revokes a board it accepted, as `tiller peer revoke` does.

const LINK_TEXT: Record<LinkState, string> = {
  live: "Open",
  lagging: "Lagging",
  down: "Down",
  refused: "Refused",
  never: "Not open yet",
};

// "SHA256:Qm4x…r9Tk": enough to tell two keys apart at a glance.
function shortKey(fingerprint: string): string {
  const [algo, hash] = fingerprint.includes(":")
    ? (fingerprint.split(":", 2) as [string, string])
    : ["", fingerprint];
  const short = hash.length > 10 ? `${hash.slice(0, 4)}…${hash.slice(-4)}` : hash;
  return algo ? `${algo}:${short}` : short;
}

const scopeText = (scope: PeerScope) => (scope === "act" ? "Act" : "Read only");

const SCOPE_MEANING: Record<PeerScope, string> = {
  act: "the board may start and stop agents there",
  read: "the board shows its rows and changes nothing there",
};

function refreshLinks(queryClient: ReturnType<typeof useQueryClient>) {
  void queryClient.invalidateQueries({ queryKey: queryKeyFor(peersPath) });
  void queryClient.invalidateQueries({ queryKey: queryKeyFor(machinesPath) });
}

// The live key a host's board holds here, when that host paired back.
function keyFrom(list: PeerList | undefined, hostId: string | null): AcceptedKey | null {
  if (!list || !hostId) return null;
  return list.accepted.find((key) => key.viewer_host_id === hostId && !key.revoked_at) ?? null;
}

type Open = { kind: "unpair" } | { kind: "revoke"; key: AcceptedKey };

function agentsText(machine: Machine): string {
  const count = machine.running_agents;
  if (count === null) return `${machine.label} has not reported its agents.`;
  if (count === 0) return `No agents run on ${machine.label} now.`;
  return `${count === 1 ? "1 agent runs" : `${count} agents run`} on ${machine.label} now.`;
}

// A paired host's card: the link this board holds to it, with Unpair on the
// right and, when its board reaches this host too, Revoke key to its left.
// The owner's, as the list is. An open dialog outlives the row it came from,
// so a reply still shows when the link ended meanwhile.
export function PeerLink({ machine, board }: { machine: Machine; board: string }) {
  const list = useQuery(peersQuery()).data;
  const [open, setOpen] = useState<Open | null>(null);
  const close = useCallback(() => setOpen(null), []);
  const paired = list?.paired.find((peer) => peer.label === machine.label);
  const dialog =
    open?.kind === "unpair" ? (
      <UnpairDialog label={machine.label} onClose={close} />
    ) : open?.kind === "revoke" ? (
      <RevokeDialog accepted={open.key} board={board} onClose={close} />
    ) : null;
  if (!paired) return dialog;
  const back = keyFrom(list, paired.peer_host_id);
  const hello = formatLisbon(machine.last_seen_at);
  const meta = [machine.link ? LINK_TEXT[machine.link] : null, hello ? `last hello ${hello}` : null]
    .filter(Boolean)
    .join(" · ");
  return (
    <HostCardSection title="Peer link" meta={meta || undefined}>
      <dl className="set-facts set-link-facts" data-testid="peer-link">
        <dt>Key</dt>
        {paired.fingerprint ? (
          <dd>
            <span className="mono" title={paired.fingerprint}>
              {shortKey(paired.fingerprint)}
            </span>
          </dd>
        ) : (
          <dd className="bad">Missing: its key file does not read</dd>
        )}
        <dt>Scope</dt>
        <dd>
          <span className="v">{scopeText(paired.scope)}</span>
          <span className="meta">{SCOPE_MEANING[paired.scope]}</span>
        </dd>
        {back ? (
          <>
            <dt>Its board</dt>
            <dd>
              <span className="v">Reaches {board} too</span>
              <span className="meta">
                {scopeText(back.scope)}, accepted {formatLisbonDay(back.accepted_at)}
              </span>
            </dd>
          </>
        ) : null}
      </dl>
      <div className="set-hfoot">
        <span className="set-hnote">{agentsText(machine)}</span>
        {back ? (
          <button
            type="button"
            className="btn"
            onClick={() => setOpen({ kind: "revoke", key: back })}
          >
            <KeyIcon />
            Revoke key...
          </button>
        ) : null}
        <button type="button" className="btn" onClick={() => setOpen({ kind: "unpair" })}>
          <UnlinkIcon />
          Unpair...
        </button>
      </div>
      {dialog}
    </HostCardSection>
  );
}

// Unpair: the host leaves this board and its key file goes. The label typed
// again arms it, as a project delete is armed.
function UnpairDialog({ label, onClose }: { label: string; onClose: () => void }) {
  const queryClient = useQueryClient();
  const ids = useId();
  const [typed, setTyped] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("DELETE", `/api/v1/peers/${encodeURIComponent(label)}`, { idempotency_key }),
    [label],
  );
  const recorded = useCallback(() => {
    refreshLinks(queryClient);
    onClose();
  }, [onClose, queryClient]);
  const action = useAction({ binding: `peer-unpair:${label}`, send, onRecorded: recorded });
  const armed = typed.trim() === label && !action.pending;
  useEffect(() => {
    input.current?.focus();
  }, []);
  return (
    <Modal
      className="dialog"
      labelledBy={`${ids}-title`}
      describedBy={`${ids}-body`}
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={input}
    >
      <h2 id={`${ids}-title`}>Unpair {label}?</h2>
      <p id={`${ids}-body`}>
        {label} leaves this board and its key file is deleted. Its projects, cards and agents stay
        on {label}. Pair it again with Add a host.
      </p>
      <div className="delete-confirm">
        <label htmlFor={`${ids}-type`}>
          Type <code>{label}</code> to confirm
        </label>
        <input
          ref={input}
          id={`${ids}-type`}
          className="note-in"
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
          value={typed}
          disabled={action.pending}
          onChange={(event) => setTyped(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && armed) action.trigger();
          }}
        />
      </div>
      {action.error ? (
        <p className="refusal" role="alert">
          {action.error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button type="button" className="btn" disabled={action.pending} onClick={onClose}>
          Cancel
        </button>
        <button type="button" className="btn danger" disabled={!armed} onClick={action.trigger}>
          {action.pending ? "Unpairing..." : "Unpair host"}
        </button>
      </div>
    </Modal>
  );
}

// Revoke key: that board stops reaching this host at once, and the key never
// comes back. Cancel takes focus.
function RevokeDialog({
  accepted,
  board,
  onClose,
}: {
  accepted: AcceptedKey;
  board: string;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const ids = useId();
  const cancel = useRef<HTMLButtonElement>(null);
  const viewer = accepted.viewer_label;
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<AcceptedKey>("DELETE", `/api/v1/peer-keys/${accepted.id}`, {
        idempotency_key,
      }),
    [accepted.id],
  );
  const recorded = useCallback(() => {
    refreshLinks(queryClient);
    onClose();
  }, [onClose, queryClient]);
  const action = useAction({ binding: `peer-revoke:${accepted.id}`, send, onRecorded: recorded });
  return (
    <Modal
      className="dialog"
      labelledBy={`${ids}-title`}
      describedBy={`${ids}-body`}
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={cancel}
    >
      <h2 id={`${ids}-title`}>Revoke {viewer}&apos;s key?</h2>
      <p id={`${ids}-body`}>
        {viewer}&apos;s board stops reaching {board} at once. Agents here keep running. To let it
        back, add {board} again from {viewer}&apos;s board.
      </p>
      {action.error ? (
        <p className="refusal" role="alert">
          {action.error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        <button
          ref={cancel}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Cancel
        </button>
        <button
          type="button"
          className="btn danger"
          disabled={action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Revoking..." : "Revoke key"}
        </button>
      </div>
    </Modal>
  );
}

function AcceptedLine({ accepted, board }: { accepted: AcceptedKey; board: string }) {
  const [confirming, setConfirming] = useState(false);
  const close = useCallback(() => setConfirming(false), []);
  const detail = accepted.revoked_at
    ? [scopeText(accepted.scope), `revoked ${formatLisbonDay(accepted.revoked_at)}`]
    : [
        scopeText(accepted.scope),
        `key ${shortKey(accepted.fingerprint)}`,
        `accepted ${formatLisbonDay(accepted.accepted_at)}`,
      ];
  return (
    <div
      className="set-row"
      data-testid={`board-key-${accepted.viewer_label}`}
      data-revoked={accepted.revoked_at ? true : undefined}
    >
      <div className="set-text">
        <h2>{accepted.viewer_label}</h2>
        <p>{detail.join(" · ")}</p>
      </div>
      <div className="set-value">
        {accepted.revoked_at ? (
          <span className="set-status missing">Revoked</span>
        ) : (
          <button type="button" className="btn" onClick={() => setConfirming(true)}>
            <KeyIcon />
            Revoke...
          </button>
        )}
      </div>
      {confirming ? <RevokeDialog accepted={accepted} board={board} onClose={close} /> : null}
    </div>
  );
}

// Settings > Boards that reach this host: the boards on other hosts whose
// keys this host accepted, live first, each with Revoke. Shown to the owner
// once a board was accepted.
export function BoardsReach({ board }: { board: string }) {
  const list = useQuery(peersQuery()).data;
  if (!list || list.accepted.length === 0) return null;
  const rows = [...list.accepted];
  rows.sort((a, b) => Number(a.revoked_at !== null) - Number(b.revoked_at !== null));
  return (
    <section
      className="set-sec"
      aria-label="Boards that reach this host"
      data-testid="boards-reach"
    >
      <div className="set-sec-h">
        <div>
          <h2>Boards that reach this host</h2>
          <p>Boards on other hosts that paired with {board} and may read or act here.</p>
        </div>
      </div>
      <div className="set-panel">
        {rows.map((accepted) => (
          <AcceptedLine key={accepted.id} accepted={accepted} board={board} />
        ))}
      </div>
    </section>
  );
}
