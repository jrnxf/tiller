import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";

import type { MutationResponse } from "../../lib/api-types";
import { machinesQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
// The form's inputs and field grid, as Add project uses them.
import "../attention/inbox.css";
import "../shell/project-stage.css";

type Scope = "act" | "read";

const ACCESS: { value: Scope; label: string }[] = [
  { value: "act", label: "Act" },
  { value: "read", label: "Read only" },
];

// What `POST /api/v1/peers` answers: the paired host row, its alias and the
// public key's fingerprint, never the key.
interface Paired {
  label: string;
  fingerprint: string | null;
}

// A daemon hint is a command-line sentence: "run ssh ... once".
function sentence(text: string) {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// `tiller peer add <label> --ssh <alias> [--read-only]` from the board: this
// host's daemon runs the new host's unattended accept over ssh (`protocol.md`
// 10.1), so the owner pairs without a terminal.
function AddHostForm({
  onClose,
  onPaired,
}: {
  onClose: () => void;
  onPaired: (p: Paired) => void;
}) {
  const queryClient = useQueryClient();
  const [label, setLabel] = useState("");
  const [alias, setAlias] = useState("");
  const [scope, setScope] = useState<Scope>("act");
  const fields = useId();
  const first = useRef<HTMLInputElement>(null);

  useEffect(() => first.current?.focus(), []);

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Paired>("POST", "/api/v1/peers", {
        label: label.trim(),
        ssh: alias.trim(),
        scope,
        idempotency_key,
      }),
    [alias, label, scope],
  );
  const recorded = useCallback(
    (response: MutationResponse<Paired>) => {
      void queryClient.invalidateQueries({ queryKey: machinesQuery().queryKey });
      onPaired(response.data);
    },
    [onPaired, queryClient],
  );
  // A refused label fixed and sent again is a new pairing; a retry of the
  // same values reuses its key.
  const action = useAction({
    binding: `host-add:${label}:${alias}:${scope}`,
    send,
    onRecorded: recorded,
  });
  const armed = label.trim() !== "" && alias.trim() !== "" && !action.pending;

  return (
    <form
      className="set-row set-form"
      aria-label="Add a host"
      onSubmit={(event) => {
        event.preventDefault();
        if (armed) action.trigger();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !action.pending) onClose();
      }}
    >
      <div className="set-text">
        <h2>Add a host</h2>
        <p>
          This host runs <code>ssh {alias.trim() || "<alias>"} tiller peer accept</code>. The alias
          must connect with no password or question, and the host must run Tiller.
        </p>
      </div>
      <div className="form-fields">
        <label htmlFor={`${fields}-label`}>Label</label>
        <input
          ref={first}
          id={`${fields}-label`}
          className="note-in mono"
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
          placeholder="host-b"
          value={label}
          disabled={action.pending}
          onChange={(event) => setLabel(event.target.value)}
        />
        <label htmlFor={`${fields}-ssh`}>SSH alias</label>
        <input
          id={`${fields}-ssh`}
          className="note-in mono"
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
          placeholder="host-b.example.com"
          value={alias}
          disabled={action.pending}
          onChange={(event) => setAlias(event.target.value)}
        />
        <span className="form-label" id={`${fields}-access`}>
          Access
        </span>
        <div className="seg" role="group" aria-labelledby={`${fields}-access`}>
          {ACCESS.map((option) => (
            <button
              key={option.value}
              type="button"
              aria-pressed={scope === option.value}
              disabled={action.pending}
              onClick={() => setScope(option.value)}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>
      {action.pending ? (
        <p className="set-progress" role="status">
          <span className="set-spin" aria-hidden="true" />
          Pairing with {label.trim()} through ssh {alias.trim()}...
        </p>
      ) : null}
      {action.error ? (
        <div className="set-refusal" role="alert">
          <p>{action.error.message.replaceAll("`", "")}</p>
          {action.error.hint ? <p>{sentence(action.error.hint.replaceAll("`", ""))}</p> : null}
        </div>
      ) : null}
      <div className="set-form-actions">
        <button type="button" className="btn" disabled={action.pending} onClick={onClose}>
          Cancel
        </button>
        <button type="submit" className="btn primary" disabled={!armed}>
          {action.pending ? "Pairing..." : "Pair"}
        </button>
      </div>
    </form>
  );
}

// The last row of Other hosts: Add a host, or its form while open. Pairing
// needs the owner cookie, so a browser not signed in is told how to get it.
export function AddHost({ empty, owner }: { empty: boolean; owner: boolean }) {
  const [open, setOpen] = useState(false);
  const [paired, setPaired] = useState<Paired | null>(null);
  const opener = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => {
    setOpen(false);
    // The opener renders again on the next commit.
    requestAnimationFrame(() => opener.current?.focus());
  }, []);
  const done = useCallback((host: Paired) => {
    setPaired(host);
    setOpen(false);
  }, []);

  if (!owner) {
    return empty ? (
      <div className="set-row" data-testid="add-host">
        <div className="set-text">
          <h2>No other host is paired</h2>
          <p>Sign this browser in as the owner to add one</p>
        </div>
      </div>
    ) : null;
  }
  if (open) return <AddHostForm onClose={close} onPaired={done} />;

  let title = empty ? "No other host is paired" : "Add a host";
  let hint = "Pair a machine this host reaches through ssh, as tiller peer add does.";
  if (paired) {
    title = `Paired ${paired.label}`;
    hint = paired.fingerprint
      ? `Its link opens within a few seconds. Key ${paired.fingerprint}`
      : "Its link opens within a few seconds.";
  }
  return (
    <div className="set-row set-action" data-testid="add-host">
      <div className="set-text">
        <h2>{title}</h2>
        <p role={paired ? "status" : undefined}>{hint}</p>
      </div>
      <button
        ref={opener}
        type="button"
        className="btn"
        onClick={() => {
          setPaired(null);
          setOpen(true);
        }}
      >
        Add a host
      </button>
    </div>
  );
}
