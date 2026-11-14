import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import type { Machine, MutationResponse, Project } from "../../lib/api-types";
import { peerMachines, standingOf } from "../../lib/peers";
import { projectCheckoutQuery, projectsQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { useMachineRows } from "../board/peer-state";
import { MinusIcon, PlusIcon } from "./icons";
import { Segmented } from "../ui/segmented";
import { Select } from "../ui/select";
import { ProjectDialog, ProjectFailure, readFailure } from "./project-dialog";

// `tiller project add`'s delivery modes, the one CP5 uses first.
const DELIVERIES = [
  "direct-pr",
  "local-only",
  "no-mistakes",
  "no-mistakes-prod-only",
  "direct-push",
] as const;

// The daemon's own slot count when none is given.
const DEFAULT_CAPACITY = 2;

// How long the checkout check waits for typing to stop.
export const CHECK_DELAY_MS = 300;

// What happens to the project's local checkout: an existing folder, a clone
// into a new one, or nothing yet (`tiller config set checkouts.<id>` later).
type CheckoutMode = "use" | "clone" | "later";

const CHECKOUT_MODES: { value: CheckoutMode; label: string }[] = [
  { value: "use", label: "Existing folder" },
  { value: "clone", label: "Clone" },
  { value: "later", label: "Later" },
];

// Where a project can be added: this host first, then each paired peer. A
// peer takes the add while its link is live with the `act` scope (D22); any
// other is listed but off, and says why.
interface HostChoice {
  value: string;
  // The peer's label; absent for this host.
  host?: string;
  label: string;
  reason: string | null;
}

const HERE = "here";
const PEER = "peer:";

// The peer a choice's value names; undefined for this host.
function peerOf(value: string): string | undefined {
  return value.startsWith(PEER) ? value.slice(PEER.length) : undefined;
}

// A peer unpaired while the dialog is open stays chosen, and off, so the add
// is not sent to a host the owner did not pick.
export function hostChoices(machines: Machine[], hostValue: string): HostChoice[] {
  const local = machines.find((machine) => machine.kind === "local")?.label ?? null;
  const peers = peerMachines(machines).map((machine): HostChoice => {
    const standing = standingOf({ host: machine.label, link: machine.link }, machines);
    return {
      value: `${PEER}${machine.label}`,
      host: machine.label,
      label: machine.label,
      reason: standing.canAct ? null : standing.reason,
    };
  });
  const host = peerOf(hostValue);
  const gone =
    host && !peers.some((choice) => choice.value === hostValue)
      ? [{ value: hostValue, host, label: host, reason: "no longer paired" }]
      : [];
  return [
    { value: HERE, label: local ? `${local} (this host)` : "This host", reason: null },
    ...peers,
    ...gone,
  ];
}

// "host-b is unreachable since Fri 13:02" says its label already; "view only
// pairing" does not.
function choiceLabel(choice: HostChoice): string {
  const { label, reason } = choice;
  if (!reason) return label;
  return reason.startsWith(`${label} `) ? reason : `${label}: ${reason}`;
}

// The seconds since `running` turned true, counted while it stays true.
function useElapsed(running: boolean): number {
  const [seconds, setSeconds] = useState(0);
  const [was, setWas] = useState(running);
  if (was !== running) {
    setWas(running);
    if (running) setSeconds(0);
  }
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setSeconds((count) => count + 1), 1000);
    return () => clearInterval(timer);
  }, [running]);
  return seconds;
}

// "0:07", "4:32".
function clock(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

// `value`, once it has stayed the same for `ms`.
function useSettled(value: string, ms: number): string {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return settled;
}

// The fields `tiller project add` takes that the board asks for, and the
// local checkout `tiller config set checkouts.<id>` would set. The rest
// (checks, remote, default branch, deploy) stay on the command line. With a
// peer chosen as the host, the folder is checked and the add runs on that
// peer, under its own checkout rules (D22).
function AddDialog({ onClose }: { onClose: () => void }) {
  const queryClient = useQueryClient();
  const [hostValue, setHostValue] = useState(HERE);
  const host = peerOf(hostValue);
  const choices = hostChoices(useMachineRows(), hostValue);
  const chosen = choices.find((choice) => choice.value === hostValue);
  const hostOff = chosen?.reason ?? null;
  const [id, setId] = useState("");
  const [repo, setRepo] = useState("");
  const [mode, setMode] = useState<CheckoutMode>("use");
  const [path, setPath] = useState("");
  const [delivery, setDelivery] = useState<string>(DELIVERIES[0]);
  const [capacity, setCapacity] = useState(DEFAULT_CAPACITY);
  const fields = useId();
  const first = useRef<HTMLInputElement>(null);
  // Why a re-added project's cards do not start at 1, or why the checkout
  // was not set: the dialog stays open to say so.
  const [added, setAdded] = useState<string | null>(null);
  const done = useRef<HTMLButtonElement>(null);

  // The daemon checks the folder as the owner types, by the same rules the
  // add applies, so a refusal shows before Add project is pressed.
  const wanted = {
    project: id.trim(),
    repo: repo.trim(),
    path: path.trim(),
    clone: mode === "clone",
    host,
  };
  // Settled as a string: a new object each render would never settle.
  const wantedKey = JSON.stringify(wanted);
  const settledKey = useSettled(wantedKey, CHECK_DELAY_MS);
  const settled = useMemo(() => JSON.parse(settledKey) as typeof wanted, [settledKey]);
  const check = useQuery({
    ...projectCheckoutQuery(settled),
    enabled: mode !== "later" && hostOff === null,
    placeholderData: keepPreviousData,
  });
  const current = settledKey === wantedKey && !check.isPlaceholderData && !check.isFetching;
  const verdict = check.data;
  const checkFailed = readFailure(check.error, "The folder could not be checked.");
  // Nothing typed yet: the default folder needs an id, which is no problem yet.
  const untouched = wanted.project === "" && wanted.path === "";
  const problem = untouched ? null : (checkFailed ?? verdict?.problem ?? null);

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Project>("POST", "/api/v1/projects", {
        id: id.trim(),
        repo: repo.trim() || null,
        delivery,
        capacity,
        checkout: mode === "later" ? null : { path: path.trim(), clone: mode === "clone" },
        host: peerOf(hostValue),
        idempotency_key,
      }),
    [capacity, delivery, hostValue, id, mode, path, repo],
  );
  const recorded = useCallback(
    (response: MutationResponse<Project>) => {
      void queryClient.invalidateQueries({ queryKey: projectsQuery().queryKey });
      if (response.warning) setAdded(response.warning);
      else onClose();
    },
    [onClose, queryClient],
  );
  // Each set of values is its own click: a refused id fixed and sent again
  // is a new add, while a retry of the same values reuses its key.
  const action = useAction({
    binding: `project-add:${hostValue}:${id}:${repo}:${mode}:${path}:${delivery}:${capacity}`,
    send,
    onRecorded: recorded,
  });
  const checkoutReady =
    mode === "later" || (current && checkFailed === null && verdict?.problem === null);
  const armed = id.trim() !== "" && hostOff === null && checkoutReady && !action.pending;
  // A clone can take up to 10 min: the dialog counts the time, so it never
  // looks stuck.
  const cloning = action.pending && mode === "clone";
  const elapsed = useElapsed(cloning);
  const submit = () => {
    if (armed) action.trigger();
  };

  if (added !== null) {
    return (
      <ProjectDialog name="add" pending={false} onClose={onClose} initial={done}>
        <h2 id="add-dialog-title">Project added</h2>
        <p className="delete-note" role="status" id="add-dialog-body">
          {added}
        </p>
        <div className="dialog-actions">
          <button ref={done} type="button" className="btn primary" onClick={onClose}>
            Done
          </button>
        </div>
      </ProjectDialog>
    );
  }

  return (
    <ProjectDialog
      name="add"
      className="pa-dialog"
      pending={action.pending}
      onClose={onClose}
      initial={first}
    >
      <h2 id="add-dialog-title">Add project</h2>
      <p id="add-dialog-body">
        {host ? (
          <>
            Registers a project on {host}, as <code>tiller project add</code> does there.
          </>
        ) : (
          <>
            Registers a project on this host, as <code>tiller project add</code> does.
          </>
        )}
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <div className="pa-fields">
          {choices.length > 1 ? (
            <div className="pa-f">
              <label className="pa-l" htmlFor={`${fields}-host`}>
                Host
              </label>
              <Select
                id={`${fields}-host`}
                className="form-select"
                value={hostValue}
                disabled={action.pending}
                onValueChange={setHostValue}
                options={choices.map((choice) => ({
                  value: choice.value,
                  label: choiceLabel(choice),
                  disabled: choice.reason !== null,
                }))}
              />
              {hostOff && chosen ? (
                <p className="pa-help" data-problem="true" role="alert">
                  {choiceLabel(chosen)}
                </p>
              ) : null}
            </div>
          ) : null}
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${fields}-id`}>
              Id
            </label>
            <input
              ref={first}
              id={`${fields}-id`}
              className="note-in mono"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              placeholder="my-app"
              aria-describedby={`${fields}-id-help`}
              value={id}
              disabled={action.pending}
              onChange={(event) => setId(event.target.value)}
            />
            <p className="pa-help" id={`${fields}-id-help`}>
              Also its name. Lowercase words and hyphens.
            </p>
          </div>
          <div className="pa-f">
            <label className="pa-l" htmlFor={`${fields}-repo`}>
              Repo <span className="pa-opt">Optional</span>
            </label>
            <input
              id={`${fields}-repo`}
              className="note-in mono"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              placeholder="owner/name"
              value={repo}
              disabled={action.pending}
              onChange={(event) => setRepo(event.target.value)}
            />
          </div>
          <div className="pa-f">
            <div className="pa-l-row">
              <label className="pa-l" htmlFor={mode === "later" ? undefined : `${fields}-path`}>
                Local checkout
              </label>
              <Segmented
                className="pa-seg"
                label="Where the checkout comes from"
                options={CHECKOUT_MODES}
                value={mode}
                disabled={action.pending}
                onChange={setMode}
              />
            </div>
            {mode === "later" ? (
              <p className="pa-help">
                Set it {host ? `on ${host} ` : ""}before the first builder with{" "}
                <code>tiller config set checkouts.{id.trim() || "<id>"}</code>.
              </p>
            ) : (
              <>
                <input
                  id={`${fields}-path`}
                  className="note-in mono"
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  placeholder={
                    path.trim() === "" && verdict?.path
                      ? verdict.path
                      : host
                        ? `checkouts_root/my-app on ${host}`
                        : "/home/user/src/my-app"
                  }
                  aria-describedby={`${fields}-path-help`}
                  aria-invalid={current && problem !== null ? true : undefined}
                  value={path}
                  disabled={action.pending}
                  onChange={(event) => setPath(event.target.value)}
                />
                <CheckoutVerdict
                  id={`${fields}-path-help`}
                  mode={mode}
                  problem={problem}
                  path={verdict?.path ?? null}
                  remote={verdict?.remote ?? null}
                  plan={verdict?.plan ?? null}
                  stale={!current}
                />
              </>
            )}
          </div>
          <div className="pa-row">
            <div className="pa-f">
              <label className="pa-l" htmlFor={`${fields}-delivery`}>
                Delivery
              </label>
              <Select
                id={`${fields}-delivery`}
                className="form-select"
                value={delivery}
                disabled={action.pending}
                onValueChange={setDelivery}
                options={DELIVERIES.map((value) => ({ value, label: value }))}
              />
            </div>
            <div className="pa-f">
              <span className="pa-l" id={`${fields}-slots`}>
                Builder slots
              </span>
              <div className="pa-num" role="group" aria-labelledby={`${fields}-slots`}>
                <button
                  type="button"
                  aria-label="One slot fewer"
                  disabled={action.pending || capacity <= 1}
                  onClick={() => setCapacity((slots) => Math.max(1, slots - 1))}
                >
                  <MinusIcon />
                </button>
                <output aria-live="polite">{capacity}</output>
                <button
                  type="button"
                  aria-label="One slot more"
                  disabled={action.pending}
                  onClick={() => setCapacity((slots) => slots + 1)}
                >
                  <PlusIcon />
                </button>
              </div>
            </div>
          </div>
        </div>
        {cloning ? (
          <p className="delete-note pa-progress" role="status">
            Cloning{host ? ` on ${host}` : ""}: {clock(elapsed)} so far. A clone can take up to 10
            min.
          </p>
        ) : null}
        <ProjectFailure error={action.error} />
        <div className="dialog-actions">
          <button type="button" className="btn" disabled={action.pending} onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn primary" disabled={!armed}>
            {action.pending ? (mode === "clone" ? "Cloning..." : "Adding...") : "Add project"}
          </button>
        </div>
      </form>
    </ProjectDialog>
  );
}

// The line under the checkout's folder: what the add will do with it, or why
// the daemon would refuse it.
function CheckoutVerdict({
  id,
  mode,
  problem,
  path,
  remote,
  plan,
  stale,
}: {
  id: string;
  mode: CheckoutMode;
  problem: string | null;
  path: string | null;
  remote: string | null;
  plan: "use" | "clone" | null;
  stale: boolean;
}) {
  let text: ReactNode;
  if (problem !== null) text = problem;
  else if (plan === "clone" && path) {
    text = (
      <>
        Clones <code>{remote}</code> into <code>{path}</code>.
      </>
    );
  } else if (plan === "use" && path) {
    text = (
      <>
        Uses the checkout at <code>{path}</code>.
      </>
    );
  } else {
    text =
      mode === "clone"
        ? "A new or empty folder. Empty means the default."
        : "A git checkout of the repo. Empty means the default.";
  }
  return (
    <p
      className="pa-help"
      id={id}
      data-problem={problem !== null ? "true" : undefined}
      data-stale={stale ? "true" : undefined}
      aria-live="polite"
    >
      {text}
    </p>
  );
}

// The "+" in the sidebar's Projects header.
export function AddProject() {
  const [open, setOpen] = useState(false);
  // The dialog gives focus back to the button that opened it.
  const close = useCallback(() => {
    setOpen(false);
  }, []);
  return (
    <>
      <button
        type="button"
        className="grp-add"
        title="Add project"
        aria-label="Add project"
        onClick={() => setOpen(true)}
      >
        <PlusIcon />
      </button>
      {open ? <AddDialog onClose={close} /> : null}
    </>
  );
}
