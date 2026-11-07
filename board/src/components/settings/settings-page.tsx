import { useState } from "react";
import type { ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import type { DaemonHealth, Machine, MergeAppHealth } from "../../lib/api-types";
import { machineRowView, VERSION_MARK_WORD } from "../../lib/machines";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import type { Density, ThemePreference } from "../../lib/prefs";
import { healthQuery, machinesQuery, sessionQuery } from "../../lib/queries";
import { signOut } from "../../lib/session";
import { useStore } from "../../lib/store";
import { formatAge, secondsSince } from "../board/format";
import { AddHost } from "./add-host";
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
      <div className="seg" role="group" aria-label={label}>
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={value === option.value}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        ))}
      </div>
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

const MERGE_APP_LABEL: Record<MergeAppHealth["status"], string> = {
  ok: "Ready",
  missing: "Not set up",
  warn: "Not used",
  fail: "Key exposed",
};

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

// What `tiller --version`, the service's start time and `tiller doctor`'s merge
// App line tell the owner, read from the daemon serving this board.
function HostFacts({ health }: { health: DaemonHealth }) {
  const up = secondsSince(health.started_at);
  const started = new Date(health.started_at);
  const { merge_app: app } = health;
  return (
    <section className="set-panel" aria-label="This host" data-testid="host-facts">
      <Fact label="Version" hint={`Schema ${health.schema}`}>
        <code>{health.version}</code>
      </Fact>
      <Fact
        label="Started"
        hint={`${up === null ? "Running" : `Up ${formatAge(up)}`}, process ${health.pid}`}
      >
        {Number.isNaN(started.getTime()) ? "unknown" : startedAt.format(started)}
      </Fact>
      <Fact label="Merge App" hint={app.detail}>
        <span className={`set-status ${app.status}`}>
          {app.status === "ok" && app.app_id !== null
            ? `App ${app.app_id}`
            : MERGE_APP_LABEL[app.status]}
        </span>
      </Fact>
    </section>
  );
}

function hostHint(machine: Machine, peer: boolean, mark: string | null, boardVersion: string) {
  if (mark) return `${mark} (${boardVersion})`;
  if (machine.version === boardVersion) return "Same as this host";
  if (machine.version) return "Runs another version";
  return peer ? "Paired peer, has not reported a version" : "herdr machine, no version to read";
}

// Each other host's Tiller version beside the board host's, so one look shows
// whether a release reached every machine, then Add a host.
function HostVersions({
  machines,
  boardVersion,
  owner,
}: {
  machines: Machine[];
  boardVersion: string;
  owner: boolean;
}) {
  const others = machines.filter((machine) => machine.kind !== "local");
  return (
    <section className="set-panel" aria-label="Other hosts" data-testid="host-versions">
      {others.map((machine) => {
        const view = machineRowView(machine, undefined, boardVersion);
        const mark = view.versionMark ? VERSION_MARK_WORD[view.versionMark] : null;
        return (
          <Fact
            key={machine.id}
            label={machine.label}
            hint={hostHint(machine, view.peer, mark, boardVersion)}
          >
            {machine.version ? (
              <>
                {mark ? <span className="set-status warn">{view.versionMark}</span> : null}
                <code>{machine.version}</code>
              </>
            ) : (
              <span className="set-muted">Not reported</span>
            )}
          </Fact>
        );
      })}
      <AddHost empty={others.length === 0} owner={owner} />
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
      <section className="set-panel" aria-label="This browser" data-testid="browser-session">
        <Fact label="Owner" hint="Run tiller board --login to sign this browser in">
          <span className="set-status missing">Not signed in</span>
        </Fact>
      </section>
    );
  }
  return (
    <section className="set-panel" aria-label="This browser" data-testid="browser-session">
      <div className="set-row set-action">
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
    </section>
  );
}

export function SettingsPage() {
  const prefs = useStore(prefsStore);
  const health = useQuery(healthQuery()).data;
  const session = useQuery(sessionQuery()).data;
  const machines = useQuery(machinesQuery()).data?.machines;
  return (
    <div className="settings-page" data-testid="settings">
      {health ? (
        <>
          <h1 className="set-title">This host</h1>
          <HostFacts health={health} />
          {machines ? (
            <>
              <h1 className="set-title">Other hosts</h1>
              <HostVersions
                machines={machines}
                boardVersion={health.version}
                owner={session?.owner ?? false}
              />
            </>
          ) : null}
        </>
      ) : null}
      {session ? (
        <>
          <h1 className="set-title">This browser</h1>
          <BrowserSession owner={session.owner} />
        </>
      ) : null}
      <h1 className="set-title">Appearance</h1>
      <section className="set-panel" aria-label="Appearance">
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
      </section>
    </div>
  );
}
