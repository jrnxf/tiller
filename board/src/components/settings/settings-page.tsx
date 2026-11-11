import { useCallback, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import type { DaemonHealth, Machine, StartSettings } from "../../lib/api-types";
import { machineRowView } from "../../lib/machines";
import type { MachineRowView, VersionMark } from "../../lib/machines";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import type { Density, ThemePreference } from "../../lib/prefs";
import {
  doctorQuery,
  healthQuery,
  machinesQuery,
  sessionQuery,
  hostConfigQuery,
  startSettingsQuery,
} from "../../lib/queries";
import { signOut } from "../../lib/session";
import { useStore } from "../../lib/store";
import { useTick } from "../board/ago";
import { formatAge, secondsSince } from "../board/format";
import { Segmented } from "../ui/segmented";
import { Select } from "../ui/select";
import { ChevronIcon, PlusIcon } from "../shell/icons";
import { AddHostForm } from "./add-host";
import { Backups } from "./backups";
import type { Paired } from "./add-host";
import { checkLines, mergeAppProblem, needsYou, whenText } from "./check-lines";
import type { Tone } from "./check-lines";
import { HostCard } from "./host-card";
import { Fact } from "./fact";
import { HostChecks } from "./host-checks";
import { HostConfigSections } from "./host-config-sections";
import { HostEdit } from "./host-edit";
import { MergeAppSection } from "./merge-app";
import { BoardsReach, PeerLink } from "./peer-link";
import { SettingField } from "./setting-field";
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

// "up 3 h": it moves with the clock, since health is read only every few
// minutes.
function Uptime({ startedAt }: { startedAt: string }) {
  useTick();
  const up = secondsSince(startedAt);
  return up === null ? null : <small>up {formatAge(up)}</small>;
}

// One row per host, this one among them: version, start, process and status.
// A click opens the host's card under it; a row whose checks need the owner
// says how many in its status.
function HostRow({
  machine,
  board,
  health,
  owner,
  peers,
  attention,
  open,
  onToggle,
}: {
  machine: Machine;
  board: { label: string; version: string };
  health: DaemonHealth;
  owner: boolean;
  // The paired peers' labels: each knows this host by its label.
  peers: string[];
  attention: { tone: Tone; text: string } | null;
  open: boolean;
  onToggle: () => void;
}) {
  const local = machine.kind === "local";
  const version = local ? health.version : (machine.version ?? null);
  const view = machineRowView(machine, undefined, board.version);
  const started = whenText(health.started_at);
  const card = `host-card-${machine.id}`;
  return (
    <>
      <tr
        data-testid={`host-${machine.label}`}
        className="set-host-row"
        data-open={open || undefined}
        onClick={onToggle}
      >
        <th scope="row" className="set-host">
          <button
            type="button"
            className="set-host-toggle"
            aria-expanded={open}
            aria-controls={open ? card : undefined}
            onClick={(event) => {
              event.stopPropagation();
              onToggle();
            }}
          >
            <ChevronIcon />
            {machine.label}
          </button>
        </th>
        <td className="set-ver" data-label="Version">
          {version ? (
            <>
              <span>{version}</span>
              {view.versionMark ? (
                <span className="set-status warn">{view.versionMark}</span>
              ) : null}
              {local ? <small>Schema {health.schema}</small> : null}
            </>
          ) : (
            <span className="set-muted">Not reported</span>
          )}
        </td>
        <td className="set-start" data-label="Started" data-empty={local ? undefined : "true"}>
          {local ? (
            <>
              {started ?? "unknown"}
              <Uptime startedAt={health.started_at} />
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
          {attention ? (
            <>
              <span className={`set-dot ${attention.tone}`} aria-hidden="true" />
              <span data-testid="host-needs-you">{attention.text}</span>
            </>
          ) : (
            <>
              <span className={`set-dot ${view.tone}`} aria-hidden="true" />
              <span title={view.tooltip ?? undefined}>{hostStatus(machine, view, board)}</span>
            </>
          )}
        </td>
      </tr>
      {open ? (
        <tr className="set-card-row">
          <td colSpan={5}>
            <HostCard id={card} host={machine.label}>
              <HostChecks
                machine={machine}
                peer={view.peer}
                board={board.label}
                health={health}
                owner={owner}
              />
              {local ? <HostEdit owner={owner} peers={peers} /> : null}
              {!local && owner ? <PeerLink machine={machine} board={board.label} /> : null}
            </HostCard>
          </td>
        </tr>
      ) : null}
    </>
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
  const [opened, setOpened] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = (id: string) =>
    setOpened((before) => {
      const next = new Set(before);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  // The serving host's checks, for the count its row shows while closed.
  const doctor = useQuery(doctorQuery());
  // The Merge App section's problems count once here, though only that
  // section shows them.
  const attention = needsYou([
    ...checkLines(doctor.data, health.pollers ?? []),
    ...mergeAppProblem(health.merge_app, doctor.data?.merge_gates ?? []),
  ]);
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
  // The paired peers, by label, as `sorted` holds them.
  const peers = sorted.filter((machine) => machine.peer_scope).map((machine) => machine.label);
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
              <HostRow
                key={machine.id}
                machine={machine}
                board={board}
                health={health}
                owner={owner}
                peers={peers}
                attention={machine.kind === "local" ? attention : null}
                open={opened.has(machine.id)}
                onToggle={() => toggle(machine.id)}
              />
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

const problemOf = (settings: StartSettings, key: string) =>
  settings.problems.find((problem) => problem.key === key)?.problem ?? null;

// The two folders every orchestrator start reads from a host's config.toml,
// set here as `tiller config set` does. The project's checkout is set where
// the project is edited. The section follows the host picker: another host's
// rows give the command to run there, since the peer link does not carry a
// config write (authority.md section 3).
function StartSettingsSection({
  settings,
  owner,
  host,
  viewing,
  picker,
}: {
  settings: StartSettings;
  owner: boolean;
  host: string | null;
  // Whose config the section shows.
  viewing: string;
  picker: ReactNode;
}) {
  const here = host ?? "this host";
  const other = viewing === here ? null : viewing;
  const rows = [
    {
      key: "spawn.pool_root",
      label: "Worktree pool",
      value: settings.pool_root,
      hint: "Where the worktrees agents work in are kept",
      placeholder: "/home/user/pool",
    },
    {
      key: "spawn.checkouts_root",
      label: "Checkouts folder",
      value: settings.checkouts_root,
      hint: "Where each project's checkout lives, as <folder>/<project>",
      placeholder: "/home/user/checkouts",
    },
  ];
  return (
    <section className="set-sec" aria-label="Starting agents" data-testid="start-settings">
      <div className="set-sec-h">
        <div>
          <h2>Starting agents</h2>
          <p>What a project&apos;s orchestrator needs before it can start on {other ?? here}.</p>
        </div>
        {picker}
      </div>
      <div className="set-panel">
        {other ? (
          rows.map((row) => (
            <SettingField
              key={row.key}
              settingKey={row.key}
              label={row.label}
              hint={row.hint}
              value={null}
              problem={null}
              valueName="path"
              host={other}
            />
          ))
        ) : settings.config_error ? (
          <Fact label="config.toml" hint="Fix the file, then reload this page">
            <span className="set-status fail">{settings.config_error.replaceAll("`", "")}</span>
          </Fact>
        ) : (
          rows.map((row) =>
            owner ? (
              <SettingField
                key={row.key}
                settingKey={row.key}
                label={row.label}
                hint={row.hint}
                value={row.value}
                problem={problemOf(settings, row.key)}
                placeholder={row.placeholder}
              />
            ) : (
              <Fact key={row.key} label={row.label} hint={row.hint}>
                {row.value ? (
                  <code>{row.value}</code>
                ) : (
                  <span className="set-status missing">Not set</span>
                )}
              </Fact>
            ),
          )
        )}
        {!other && settings.orchestrator_home ? (
          <Fact label="Orchestrator homes" hint="Follows from the folders above">
            <code>{settings.orchestrator_home}</code>
          </Fact>
        ) : null}
        {!other && settings.scratch_root ? (
          <Fact label="Run scratch" hint="Follows from the folders above">
            <code>{settings.scratch_root}</code>
          </Fact>
        ) : null}
        {owner || other ? null : (
          <Fact label="Change these" hint="Sign this browser in as the owner">
            <span className="set-status missing">Not signed in</span>
          </Fact>
        )}
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
  const startSettings = useQuery(startSettingsQuery()).data;
  const hostConfig = useQuery(hostConfigQuery()).data;
  const host = machines?.find((machine) => machine.kind === "local")?.label ?? null;
  // Whose config the host sections show: this host's, or a peer's command.
  const [chosen, setChosen] = useState<string | null>(null);
  const hosts = (machines ?? []).map((machine) => machine.label);
  const viewing = chosen !== null && hosts.includes(chosen) ? chosen : (host ?? "this host");
  // One picker heads the config.toml sections, which follow it together.
  const picker =
    hosts.length > 1 ? (
      <Select
        label="Host"
        className="form-select"
        value={viewing}
        options={hosts.map((label) => ({ value: label, label }))}
        onValueChange={setChosen}
      />
    ) : null;
  return (
    <div className="settings-page" data-testid="settings">
      {/* These four take no host picker. Hosts already lists every host. The
          Merge App, Boards that reach this host and Backups read this
          daemon's own health, accepted keys and backups, and their actions
          are never forwarded (protocol.md 7.3 and 7.4), so each names this
          host instead. */}
      {health ? <MergeAppSection app={health.merge_app} host={host} /> : null}
      {health ? (
        <Hosts machines={machines} health={health} owner={session?.owner ?? false} />
      ) : null}
      {session?.owner ? <BoardsReach board={host ?? "this host"} /> : null}
      {session?.owner ? <Backups host={host} /> : null}
      {startSettings ? (
        <StartSettingsSection
          settings={startSettings}
          owner={session?.owner ?? false}
          host={host}
          viewing={viewing}
          picker={picker}
        />
      ) : null}
      {hostConfig ? (
        <HostConfigSections
          config={hostConfig}
          owner={session?.owner ?? false}
          host={host}
          viewing={viewing}
          picker={startSettings ? null : picker}
        />
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
