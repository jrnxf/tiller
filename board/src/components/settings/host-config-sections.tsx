import type { ReactNode } from "react";

import type { HostConfig } from "../../lib/api-types";
import { Select } from "../ui/select";
import { Fact } from "./fact";
import { SettingField } from "./setting-field";
import type { Control } from "./setting-field";
import "./settings.css";

interface Row {
  key: string;
  label: string;
  hint: ReactNode;
  // What config.toml holds, as the daemon reads it; null when it is not set.
  value: string | null;
  control?: Control;
  mono?: boolean;
  allowEmpty?: boolean;
  placeholder?: string;
  // What the command shows for the value on another host.
  valueName: string;
  // What a browser that cannot change it reads; defaults to the value.
  shown?: string;
}

const STOP_MODES = [
  { value: "off", label: "Off" },
  { value: "observe", label: "Observe" },
  { value: "block-once", label: "Block once" },
];

const ON_OFF = [
  { value: "true", label: "On" },
  { value: "false", label: "Off" },
];

const MODELS = ["sonnet", "opus", "haiku"].map((value) => ({ value, label: value }));

const labelOf = (options: { value: string; label: string }[], value: string) =>
  options.find((option) => option.value === value)?.label ?? value;

function agentRows(config: HostConfig): Row[] {
  return [
    {
      key: "herdr.terminal",
      label: "herdr terminal",
      hint: "The terminal app herdr opens for Open in herdr. Read when the daemon starts",
      value: config.herdr_terminal,
      mono: false,
      placeholder: "ghostty",
      valueName: "app",
    },
    {
      key: "stop_hook_mode",
      label: "Stop hook mode",
      hint: "Off, observe, or block once when a stop leaves an ask open",
      value: config.stop_hook_mode,
      control: { kind: "choice", options: STOP_MODES },
      valueName: "off|observe|block-once",
      shown: labelOf(STOP_MODES, config.stop_hook_mode),
    },
  ];
}

function peerRows(config: HostConfig): Row[] {
  const work = config.boundary === "work";
  return [
    {
      key: "federation.accept",
      label: "Accept peers",
      hint: "Off refuses new pairings and every peer link to this host",
      value: String(config.accept_peers),
      control: { kind: "choice", options: ON_OFF },
      valueName: "true|false",
      shown: config.accept_peers ? "On" : "Off",
    },
    {
      key: "memory.remote",
      label: "Memory remote",
      hint: work
        ? "A work host never pushes its memory repo, so this stays empty"
        : "The git remote memory is pushed to. Empty keeps it local",
      value: config.memory_remote,
      allowEmpty: true,
      placeholder: "git@example.com:team/memory.git",
      valueName: "address",
      shown: config.memory_remote ?? "Kept on this host",
    },
  ];
}

function learningRows(config: HostConfig): Row[] {
  const { learning } = config;
  return [
    {
      key: "learning.sample_every",
      label: "Retro sample",
      hint: "Run a retro on 1 card in every N",
      value: String(learning.sample_every),
      control: { kind: "number", unit: "cards" },
      valueName: "n",
      shown: `${learning.sample_every} cards`,
    },
    {
      key: "learning.window_cards",
      label: "Measure window",
      hint: "Cards a kept change set is measured over",
      value: String(learning.window_cards),
      control: { kind: "number", unit: "cards" },
      valueName: "n",
      shown: `${learning.window_cards} cards`,
    },
    {
      key: "learning.curator_model",
      label: "Curator model",
      hint: "The model a retro's curator runs on",
      value: learning.curator_model,
      control: { kind: "select", options: MODELS },
      valueName: "model",
    },
    {
      key: "learning.curator_timeout_minutes",
      label: "Curator limit",
      hint: "A curator still running at this point is cut",
      value: String(learning.curator_timeout_minutes),
      control: { kind: "number", unit: "min" },
      valueName: "minutes",
      shown: `${learning.curator_timeout_minutes} min`,
    },
    {
      key: "learning.evals_dir",
      label: "Evals folder",
      hint: "Where the holdout cases live: the folder that holds evals/run.sh",
      value: learning.evals_dir,
      placeholder: "/home/user/evals",
      valueName: "path",
    },
  ];
}

// One row per key: the control and Save for the owner on this host, the
// command to run for a host that keeps its own, and the value for a browser
// that cannot change it.
function Rows({
  rows,
  owner,
  other,
}: {
  rows: Row[];
  owner: boolean;
  // The peer whose config the rows stand for; null for this host.
  other: string | null;
}) {
  return rows.map((row) => {
    if (other || owner) {
      return (
        <SettingField
          key={row.key}
          settingKey={row.key}
          label={row.label}
          hint={row.hint}
          value={row.value}
          control={row.control}
          mono={row.mono}
          allowEmpty={row.allowEmpty}
          placeholder={row.placeholder}
          valueName={row.valueName}
          host={other}
        />
      );
    }
    const text = row.shown ?? row.value;
    return (
      <Fact key={row.key} label={row.label} hint={row.hint}>
        {!text ? (
          <span className="set-status missing">Not set</span>
        ) : row.control || row.mono === false || !row.value ? (
          <span>{text}</span>
        ) : (
          <code>{text}</code>
        )}
      </Fact>
    );
  });
}

function Section({
  name,
  testid,
  intro,
  action,
  children,
}: {
  name: string;
  testid: string;
  intro: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="set-sec" aria-label={name} data-testid={testid}>
      <div className="set-sec-h">
        <div>
          <h2>{name}</h2>
          <p>{intro}</p>
        </div>
        {action}
      </div>
      <div className="set-panel">{children}</div>
    </section>
  );
}

// The host keys beyond the two start folders, as `tiller config set` sets
// them: Agents and hooks, Peers and memory, Learning. They are this host's
// config.toml, so another host's rows give the command to run there; the
// peer link does not carry a config write (authority.md section 3).
export function HostConfigSections({
  config,
  owner,
  host,
  hosts,
  viewing,
  onView,
}: {
  config: HostConfig;
  owner: boolean;
  // The host serving this board, by its label; null until the list loads.
  host: string | null;
  // Every host's label, this one among them.
  hosts: string[];
  // Whose config the sections show.
  viewing: string;
  onView: (label: string) => void;
}) {
  const here = host ?? "this host";
  const other = viewing === here ? null : viewing;
  const on = other ?? here;
  if (config.config_error && !other) {
    return (
      <Section
        name="Host config"
        testid="host-config"
        intro={`The rest of config.toml on ${here}.`}
      >
        <Fact label="config.toml" hint="Fix the file, then reload this page">
          <span className="set-status fail">{config.config_error.replaceAll("`", "")}</span>
        </Fact>
      </Section>
    );
  }
  const picker =
    hosts.length > 1 ? (
      <Select
        label="Host"
        className="form-select"
        value={viewing}
        options={hosts.map((label) => ({ value: label, label }))}
        onValueChange={onView}
      />
    ) : null;
  return (
    <>
      <Section
        name="Agents and hooks"
        testid="host-config-agents"
        intro={`Read by every new agent on ${on}.`}
        action={picker}
      >
        <Rows rows={agentRows(config)} owner={owner} other={other} />
      </Section>
      <Section
        name="Peers and memory"
        testid="host-config-peers"
        intro="What other hosts may do here, and where memory is pushed."
      >
        <Rows rows={peerRows(config)} owner={owner} other={other} />
      </Section>
      <Section
        name="Learning"
        testid="host-config-learning"
        intro="How retros sample cards and how the curator runs."
      >
        <Rows rows={learningRows(config)} owner={owner} other={other} />
      </Section>
      {owner ? null : (
        <p className="set-note">Sign this browser in as the owner to change these.</p>
      )}
    </>
  );
}
