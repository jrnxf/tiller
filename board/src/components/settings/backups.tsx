import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import type { BackupRow } from "../../lib/api-types";
import { formatLisbonDay } from "../../lib/clock";
import { backupsPath, backupsQuery, queryKeyFor } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import "./settings.css";

// How many backups the list shows. The daemon keeps more.
const SHOWN = 6;

const MB = 1024 * 1024;

// "48 MB", or "1.2 GB" past a thousand; under one MB, "312 KB".
export function sizeText(bytes: number): string {
  if (bytes < MB) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  const mb = bytes / MB;
  if (mb >= 1000) return `${(mb / 1024).toFixed(1)} GB`;
  return `${Math.round(mb)} MB`;
}

// Why a copy did not verify, as a sentence: the daemon's reason, then what
// the owner can count on and do.
function failureText(error: string | null): string {
  const reason = (error ?? "the copy did not verify").replace(/[.\s]+$/, "");
  return `Not verified: ${reason}. The previous verified backup is kept. Try again.`;
}

function BackupLine({ row }: { row: BackupRow }) {
  const failed = row.status === "failed";
  const when = formatLisbonDay(row.at) ?? row.at;
  const detail = [
    when,
    row.size === null ? null : sizeText(row.size),
    row.kind === "nightly" ? "by the nightly timer" : "by you",
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <div className="set-row" data-testid="backup-row" data-status={row.status}>
      <div className="set-text">
        <h2>{row.kind === "nightly" ? "Nightly" : "By hand"}</h2>
        <p>{detail}</p>
        {failed ? <p className="set-error">{failureText(row.error)}</p> : null}
      </div>
      <div className="set-value">
        <span className={`set-status ${failed ? "fail" : "ok"}`}>
          {failed ? "Failed" : "Verified"}
        </span>
      </div>
    </div>
  );
}

// Settings > Backups: the recent backups of this host and Back up now, as
// `tiller backup` takes one. Export and restore stay in a terminal. The list
// is the owner's, so the section shows only to the owner.
export function Backups({ host }: { host: string | null }) {
  const queryClient = useQueryClient();
  const list = useQuery({
    ...backupsQuery(),
    refetchInterval: (query) => (query.state.data?.running ? 2000 : false),
  }).data;
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!list) return null;
  const running = starting || list.running;
  const refresh = () => queryClient.invalidateQueries({ queryKey: queryKeyFor(backupsPath) });
  const backUp = () => {
    if (running) return;
    setStarting(true);
    setError(null);
    sendMutation<BackupRow>("POST", backupsPath, { idempotency_key: ulid() })
      .catch((caught: unknown) => {
        setError(
          caught instanceof ActionError
            ? caught.message.replaceAll("`", "")
            : "The backup did not start.",
        );
      })
      .then(refresh)
      .finally(() => setStarting(false));
  };
  return (
    <section className="set-sec" aria-label="Backups" data-testid="backups">
      <div className="set-sec-h">
        <div>
          <h2>Backups</h2>
          <p>
            A verified copy of {host ? `${host}'s` : "this host's"} state, every night. 14 daily and
            8 weekly copies are kept.
          </p>
        </div>
        <button type="button" className="btn primary" disabled={running} onClick={backUp}>
          {running ? "Backing up..." : "Back up now"}
        </button>
      </div>
      <div className="set-panel">
        {error ? (
          <div className="set-row" role="alert">
            <div className="set-text">
              <p className="set-error">{error}</p>
            </div>
          </div>
        ) : null}
        {list.backups.length === 0 ? (
          <div className="set-row" data-testid="no-backups">
            <div className="set-text">
              <h2>No backup yet</h2>
              <p>The first nightly backup runs tonight. Back up now takes one at once.</p>
            </div>
          </div>
        ) : (
          list.backups.slice(0, SHOWN).map((row) => <BackupLine key={row.id} row={row} />)
        )}
        <div className="set-row">
          <div className="set-text">
            <h2>Folder</h2>
            <p>Where the copies are kept</p>
          </div>
          <div className="set-value">
            <code>{list.folder}</code>
          </div>
        </div>
        <div className="set-row">
          <div className="set-text">
            <h2>Export and restore</h2>
            <p>They write outside this folder or replace the state, so they stay in a terminal</p>
          </div>
          <div className="set-value set-commands">
            <code>tiller export --jsonl &lt;dir&gt;</code>
            <code>tiller restore &lt;file&gt;</code>
          </div>
        </div>
      </div>
    </section>
  );
}
