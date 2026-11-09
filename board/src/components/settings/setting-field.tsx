import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useId, useState } from "react";
import type { ReactNode } from "react";

import { projectSetupPath, queryKeyFor, startSettingsQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
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

// `tiller config set <key> <value>` from the board, for one of the settings an
// orchestrator start reads. The daemon checks the value as a start would and
// answers in plain words when it refuses.
export function SettingField({
  settingKey,
  label,
  hint,
  value,
  suggestion,
  problem,
  placeholder,
  host,
  onSaved,
}: {
  // `spawn.pool_root`, `spawn.checkouts_root` or `checkouts.<project>`.
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
            Set on {host}. Run <code>tiller config set {settingKey} &lt;path&gt;</code> there.
          </p>
          {problem ? <p className="set-error">{plain(problem)}</p> : null}
        </div>
      </div>
    );
  }

  const next = draft.trim();
  const armed = next !== "" && next !== value && !action.pending && !action.recorded;
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
          <label htmlFor={`${id}-input`}>{label}</label>
        </h2>
        <p>{hint}</p>
        {problem && !action.error ? <p className="set-error">{plain(problem)}</p> : null}
      </div>
      <div className="setting-control">
        <input
          id={`${id}-input`}
          className="note-in mono"
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
          placeholder={placeholder}
          value={draft}
          disabled={action.pending}
          aria-invalid={problem || action.error ? true : undefined}
          onChange={(event) => setTyped(event.target.value)}
        />
        <button type="submit" className="btn primary" disabled={!armed}>
          {action.pending ? "Saving..." : action.recorded ? "Saved" : "Save"}
        </button>
      </div>
      {action.error ? (
        <p className="set-error setting-refusal" role="alert">
          {plain(action.error.message)}
        </p>
      ) : null}
    </form>
  );
}
