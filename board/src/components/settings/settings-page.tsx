import { useCallback, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import type { DaemonHealth, Machine, MergeAppHealth } from "../../lib/api-types";
import { machineRowView } from "../../lib/machines";
import type { MachineRowView, VersionMark } from "../../lib/machines";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import type { Density, ThemePreference } from "../../lib/prefs";
import { healthQuery, machinesQuery, sessionQuery } from "../../lib/queries";
import { signOut } from "../../lib/session";
import { useStore } from "../../lib/store";
import { formatAge, secondsSince } from "../board/format";
import { Segmented } from "../ui/segmented";
import { WarnIcon } from "../board/icons";
import { PlusIcon } from "../shell/icons";
import { AddHostForm } from "./add-host";
import type { Paired } from "./add-host";
import "./settings.css";

const THEMES: { value: ThemePreference; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const DENSITIES: { value: Density; label: string }[] = [
  { value: "comfortable", label: "Comfortable" },
  { value: "compact", label: "Compact" },
];

function Choice<Value extends string>({
  label,
  hint,
  options,
  value,
  onChange,
}: {
  label: string;
  hint: string;
  options: { value: Value; label: string }[];
  value: Value;
  onChange: (value: Value) => void;
}) {
  return (
    <div className="set-row">
      <div className="set-text">
        <h2>{label}</h2>
        <p>{hint}</p>
      </div>
      <Segmented label={label} options={options} value={value} onChange={onChange} />
    </div>
  );
}

function Fact({ label, hint, children }: { label: string; hint: string; children: ReactNode }) {
  return (
    <div className="set-row">
      <div className="set-text">
        <h2>{label}</h2>
        <p>{hint}</p>
      </div>
      <div className="set-value">{children}</div>
    </div>
  );
}

// Always with the weekday: "Thu 5 Nov, 23:40".
const startedAt = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "short",
  day: "numeric",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

// The warning above the hosts when the merge App is not ready, as `tiller
// doctor`'s `merge app` line words it.
const MERGE_APP_TITLE: Record<Exclude<MergeAppHealth["status"], "ok">, string> = {
  missing: "Merge App not set up",
  warn: "Merge App not used",
  fail: "Merge App key exposed",
};

// The merge App belongs to the daemon serving this board, so it is named by
// that host's label like any other host's fact.
function MergeApp({ app, host }: { app: MergeAppHealth; host: string | null }) {
  const on = host ? ` on ${host}` : "";
  if (app.status === "ok") {
    return (
      <p className="set-note" data-testid="merge-app">
        <span className="set-dot green" aria-hidden="true" />
        {app.app_id === null ? "Merge App ready" : `Merge App ${app.app_id} ready`}
        {on}
      </p>
    );
  }
  return (
    <div className="set-alert" data-status={app.status} role="status" data-testid="merge-app">
      <WarnIcon />
      <b>
        {MERGE_APP_TITLE[app.status]}
        {on}
      </b>
      <p>{app.detail}</p>
    </div>
  );
}

// "Older than host-a (0.1.0-pre.17)": against the host serving this board,
// named by its label.
const VERSION_MARK_TEXT: Record<VersionMark, string> = {
  older: "Older than",
  newer: "Newer than",
  different: "Differs from",
};

function hostStatus(
  machine: Machine,
  view: MachineRowView,
  board: { label: string; version: string },
): string {
  if (machine.kind === "local") return "Serves this board";
  if (view.staleText) return view.staleText.charAt(0).toUpperCase() + view.staleText.slice(1);
  if (view.versionMark) {
    return `${VERSION_MARK_TEXT[view.versionMark]} ${board.label} (${board.version})`;
  }
  if (machine.version) return `Same as ${board.label}`;
  return view.peer
    ? "Paired peer, has not reported a version"
    : "herdr machine, no version to read";
}

// A start time and process only the serving daemon reports today; a peer's
// row says so instead of guessing.
const NOT_REPORTED = "Only the host serving this board reports this";

// One row per host, this one among them: version, start, process and status.
function HostRow({
  machine,
  board,
  health,
}: {
  machine: Machine;
  board: { label: string; version: string };
  health: DaemonHealth;
}) {
  const local = machine.kind === "local";
  const version = local ? health.version : (machine.version ?? null);
  const view = machineRowView(machine, undefined, board.version);
  const started = new Date(health.started_at);
  const up = secondsSince(health.started_at);
  return (
    <tr data-testid={`host-${machine.label}`}>
      <th scope="row" className="set-host">
        {machine.label}
      </th>
      <td className="set-ver" data-label="Version">
        {version ? (
          <>
            <code>{version}</code>
            {view.versionMark ? <span className="set-status warn">{view.versionMark}</span> : null}
            {local ? <small>Schema {health.schema}</small> : null}
          </>
        ) : (
          <span className="set-muted">Not reported</span>
        )}
      </td>
      <td className="set-start" data-label="Started" data-empty={local ? undefined : "true"}>
        {local ? (
          <>
            {Number.isNaN(started.getTime()) ? "unknown" : startedAt.format(started)}
            {up === null ? null : <small>up {formatAge(up)}</small>}
          </>
        ) : (
          <span className="set-muted" title={NOT_REPORTED}>
            -
          </span>
        )}
      </td>
      <td className="set-pid" data-label="Process" data-empty={local ? undefined : "true"}>
        {local ? (
          <code>{health.pid}</code>
        ) : (
          <span className="set-muted" title={NOT_REPORTED}>
            -
          </span>
        )}
      </td>
      <td className="set-st">
        <span className={`set-dot ${view.tone}`} aria-hidden="true" />
        <span title={view.tooltip ?? undefined}>{hostStatus(machine, view, board)}</span>
      </td>
    </tr>
  );
}

// The host serving this board, from its own health, while the hosts list
// loads or when it cannot be read.
const serving = (health: DaemonHealth): Machine => ({
  id: "local",
  label: "This host",
  kind: "local",
  view_only: false,
  health: "ok",
  running_agents: null,
  capacity: null,
  free_slots: null,
  last_seen_at: null,
  stale_reason: null,
  version: health.version,
  rev: 0,
});

// Every host the same way, so one look shows whether a release reached each
// machine; then Add a host, which pairs another through ssh.
function Hosts({
  machines,
  health,
  owner,
}: {
  machines: Machine[] | undefined;
  health: DaemonHealth;
  owner: boolean;
}) {
  const [adding, setAdding] = useState(false);
  const [paired, setPaired] = useState<Paired | null>(null);
  const opener = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => {
    setAdding(false);
    // The form unmounts on the next commit; focus goes back to its opener.
    requestAnimationFrame(() => opener.current?.focus());
  }, []);
  const done = useCallback((host: Paired) => {
    setPaired(host);
    setAdding(false);
  }, []);
  const local = machines?.find((machine) => machine.kind === "local");
  const board = { label: local?.label ?? "this host", version: health.version };
  const sorted = local ? [...(machines ?? [])] : [serving(health), ...(machines ?? [])];
  sorted.sort((a, b) => a.label.localeCompare(b.label));
  // Unknown until the hosts list is read.
  const alone = machines?.every((machine) => machine.kind === "local") ?? false;
  return (
    <section className="set-sec" aria-label="Hosts" data-testid="hosts">
      <div className="set-sec-h">
        <div>
          <h2>Hosts</h2>
          <p>
            {owner ? (
              <>
                Pair another machine through ssh, as <code>tiller peer add</code> does.
              </>
            ) : (
              "Sign this browser in as the owner to add one"
            )}
          </p>
        </div>
        {owner ? (
          <button
            ref={opener}
            type="button"
            className="btn"
            onClick={() => {
              setPaired(null);
              setAdding(true);
            }}
          >
            <PlusIcon />
            Add a host
          </button>
        ) : null}
      </div>
      <div className="set-panel">
        <table className="set-hosts">
          <thead>
            <tr>
              <th scope="col">Host</th>
              <th scope="col">Version</th>
              <th scope="col">Started</th>
              <th scope="col">Process</th>
              <th scope="col">Status</th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((machine) => (
              <HostRow key={machine.id} machine={machine} board={board} health={health} />
            ))}
          </tbody>
        </table>
        {adding ? <AddHostForm onClose={close} onPaired={done} /> : null}
        {!adding && paired ? (
          <div className="set-row set-foot" role="status">
            <div className="set-text">
              <h2>Paired {paired.label}</h2>
              <p>
                {paired.fingerprint
                  ? `Its link opens within a few seconds. Key ${paired.fingerprint}`
                  : "Its link opens within a few seconds."}
              </p>
            </div>
          </div>
        ) : null}
        {!adding && !paired && alone ? (
          <div className="set-row set-foot" data-testid="no-hosts">
            <div className="set-text">
              <h2>No other host is paired</h2>
              <p>Agents run only on {board.label} until another host is added.</p>
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}

// Signing out ends this browser's owner cookie on the daemon, not the other
// devices signed in with their own codes.
function BrowserSession({ owner }: { owner: boolean }) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const queryClient = useQueryClient();
  if (!owner) {
    return (
      <Fact label="Owner" hint="Run tiller board --login to sign this browser in">
        <span className="set-status missing">Not signed in</span>
      </Fact>
    );
  }
  return (
    <div className="set-row set-action" data-testid="browser-session">
      <div className="set-text">
        <h2>Signed in as the owner</h2>
        <p className={error ? "set-error" : undefined} role={error ? "alert" : undefined}>
          {error ?? "Signing out ends the sign-in on this browser only"}
        </p>
      </div>
      <button
        type="button"
        className="btn"
        disabled={pending}
        onClick={() => {
          setPending(true);
          setError(null);
          void signOut().then((result) => {
            // At an allowed host the app swaps to the signed-out screen; on
            // loopback this panel turns to "Not signed in".
            if (result.ok) {
              void queryClient.invalidateQueries({ queryKey: sessionQuery().queryKey });
            } else {
              setError(result.message.replaceAll("`", ""));
            }
            setPending(false);
          });
        }}
      >
        Sign out
      </button>
    </div>
  );
}

export function SettingsPage() {
  const prefs = useStore(prefsStore);
  const health = useQuery(healthQuery()).data;
  const session = useQuery(sessionQuery()).data;
  const machines = useQuery(machinesQuery()).data?.machines;
  const host = machines?.find((machine) => machine.kind === "local")?.label ?? null;
  return (
    <div className="settings-page" data-testid="settings">
      {health ? <MergeApp app={health.merge_app} host={host} /> : null}
      {health ? (
        <Hosts machines={machines} health={health} owner={session?.owner ?? false} />
      ) : null}
      <section className="set-sec" aria-label="This browser">
        <div className="set-sec-h">
          <div>
            <h2>This browser</h2>
            <p>Kept in this browser only.</p>
          </div>
        </div>
        <div className="set-panel">
          {session ? <BrowserSession owner={session.owner} /> : null}
          <Choice
            label="Theme"
            hint="Follow the system, or stay light or dark"
            options={THEMES}
            value={prefs.theme}
            onChange={(theme) => updatePrefs({ theme })}
          />
          <Choice
            label="Density"
            hint="How much room rows and cards take"
            options={DENSITIES}
            value={prefs.density}
            onChange={(density) => updatePrefs({ density })}
          />
        </div>
      </section>
    </div>
  );
}
