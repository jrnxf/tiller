import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useId, useState } from "react";
import type { ReactNode } from "react";

import {
  hostConfigQuery,
  projectSetupPath,
  queryKeyFor,
  startSettingsQuery,
} from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { Segmented } from "../ui/segmented";
import { Select } from "../ui/select";
import { SwitchingRule, switchingRuleOf } from "./switching-rule";
// The text input, as Add project and Add a host use it.
import "../attention/inbox.css";
import "./settings.css";

// What `POST /api/v1/config` answers.
interface Saved {
  key: string;
  value: string;
  changed: boolean;
}

// A daemon sentence carries `code` marks meant for a terminal.
const plain = (text: string) => text.replaceAll("`", "");

// How a setting is picked. A path or a name is typed; a few fixed values are
// a segmented control; a short list is a select; a count has its unit.
export type Control =
  | { kind: "text" }
  | { kind: "choice"; options: { value: string; label: string }[] }
  | { kind: "select"; options: { value: string; label: string }[] }
  | { kind: "number"; unit: string };

// `tiller config set <key> <value>` from the board, for a setting of this
// host's config.toml. The daemon checks the value as the code that reads it
// would and answers in plain words when it refuses.
export function SettingField({
  settingKey,
  label,
  hint,
  value,
  suggestion,
  problem,
  placeholder,
  host,
  control = { kind: "text" },
  mono = true,
  allowEmpty = false,
  valueName = "path",
  onSaved,
}: {
  // `spawn.pool_root`, `checkouts.<project>`, `stop_hook_mode`, ...
  settingKey: string;
  label: string;
  hint: ReactNode;
  // What config.toml holds now; null when it is not set.
  value: string | null;
  // The only default there is, filled in for an unset setting.
  suggestion?: string;
  // What a start says is wrong with the value now.
  problem?: string | null;
  placeholder?: string;
  // The peer the setting belongs to. It is written on that host, so the
  // board shows the command and no input.
  host?: string | null;
  control?: Control;
  // A name or address, not a path: the input is not set in the mono face.
  mono?: boolean;
  // An empty value is a valid save, such as no memory remote.
  allowEmpty?: boolean;
  // What the command shows for the value, `<path>` by default.
  valueName?: string;
  onSaved?: () => void;
}) {
  const queryClient = useQueryClient();
  const id = useId();
  // Until the owner types, the field follows the saved value, then the only
  // default there is.
  const [typed, setTyped] = useState<string | null>(null);
  const draft = typed ?? value ?? suggestion ?? "";
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Saved>("POST", "/api/v1/config", {
        key: settingKey,
        value: draft.trim(),
        idempotency_key,
      }),
    [draft, settingKey],
  );
  const recorded = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: startSettingsQuery().queryKey });
    void queryClient.invalidateQueries({ queryKey: hostConfigQuery().queryKey });
    const project = settingKey.startsWith("checkouts.")
      ? settingKey.slice("checkouts.".length)
      : null;
    if (project) {
      void queryClient.invalidateQueries({ queryKey: queryKeyFor(projectSetupPath(project)) });
    }
    onSaved?.();
  }, [onSaved, queryClient, settingKey]);
  // An edited value is a new save; a retry of the same value reuses its key.
  const action = useAction({
    binding: `config:${settingKey}:${value ?? ""}:${draft.trim()}`,
    send,
    onRecorded: recorded,
  });

  if (host) {
    return (
      <div className="setting-field" data-testid={`setting-${settingKey}`}>
        <div className="set-text">
          <h2>{label}</h2>
          <p>
            Set on {host}. Run{" "}
            <code>
              tiller config set {settingKey} &lt;{valueName}&gt;
            </code>{" "}
            there.
          </p>
          {problem ? <p className="set-error">{plain(problem)}</p> : null}
        </div>
      </div>
    );
  }

  const next = draft.trim();
  const armed =
    (next !== "" || allowEmpty) && next !== (value ?? "") && !action.pending && !action.recorded;
  const rule = action.error ? switchingRuleOf(action.error) : null;
  return (
    <form
      className="setting-field"
      data-testid={`setting-${settingKey}`}
      aria-label={label}
      onSubmit={(event) => {
        event.preventDefault();
        if (armed) action.trigger();
      }}
    >
      <div className="set-text">
        <h2>
          {control.kind === "text" || control.kind === "number" ? (
            <label htmlFor={`${id}-input`}>{label}</label>
          ) : (
            label
          )}
        </h2>
        <p>{hint}</p>
        {problem && !action.error ? <p className="set-error">{plain(problem)}</p> : null}
      </div>
      <div className={`setting-control${control.kind === "choice" ? " wrap" : ""}`}>
        {control.kind === "choice" ? (
          <Segmented
            label={label}
            options={control.options}
            value={draft}
            disabled={action.pending}
            onChange={setTyped}
          />
        ) : control.kind === "select" ? (
          <Select
            label={label}
            className="form-select"
            value={draft}
            disabled={action.pending}
            options={
              control.options.some((option) => option.value === draft)
                ? control.options
                : [...control.options, { value: draft, label: draft }]
            }
            onValueChange={setTyped}
          />
        ) : (
          <span className={control.kind === "number" ? "set-unit" : "set-input"}>
            <input
              id={`${id}-input`}
              className={`note-in${mono && control.kind === "text" ? " mono" : ""}`}
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              inputMode={control.kind === "number" ? "numeric" : undefined}
              placeholder={placeholder}
              value={draft}
              disabled={action.pending}
              aria-invalid={problem || action.error ? true : undefined}
              onChange={(event) => setTyped(event.target.value)}
            />
            {control.kind === "number" ? <span aria-hidden="true">{control.unit}</span> : null}
          </span>
        )}
        <button type="submit" className="btn primary" disabled={!armed}>
          {action.pending ? "Saving..." : action.recorded ? "Saved" : "Save"}
        </button>
      </div>
      {action.error && rule ? (
        <div className="set-error setting-refusal" role="alert">
          <SwitchingRule rule={rule} />
        </div>
      ) : action.error ? (
        <p className="set-error setting-refusal" role="alert">
          {plain(action.error.message)}
        </p>
      ) : null}
    </form>
  );
}
