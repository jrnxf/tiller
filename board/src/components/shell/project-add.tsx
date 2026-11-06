import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useId, useRef, useState } from "react";

import type { Project } from "../../lib/api-types";
import { projectsQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { PlusIcon } from "./icons";
import { ProjectDialog } from "./project-dialog";

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

// The fields `tiller project add` takes that the board asks for. The rest
// (checks, remote, default branch, deploy) stay on the command line.
function AddDialog({ onClose }: { onClose: () => void }) {
  const queryClient = useQueryClient();
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [repo, setRepo] = useState("");
  const [delivery, setDelivery] = useState<string>(DELIVERIES[0]);
  const [capacity, setCapacity] = useState(String(DEFAULT_CAPACITY));
  const fields = useId();
  const first = useRef<HTMLInputElement>(null);

  const slots = Number(capacity);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Project>("POST", "/api/v1/projects", {
        id: id.trim(),
        name: name.trim(),
        repo: repo.trim() || null,
        delivery,
        capacity: Number(capacity),
        idempotency_key,
      }),
    [capacity, delivery, id, name, repo],
  );
  const recorded = useCallback(() => {
    onClose();
    void queryClient.invalidateQueries({ queryKey: projectsQuery().queryKey });
  }, [onClose, queryClient]);
  // Each set of values is its own click: a refused id fixed and sent again
  // is a new add, while a retry of the same values reuses its key.
  const action = useAction({
    binding: `project-add:${id}:${name}:${repo}:${delivery}:${capacity}`,
    send,
    onRecorded: recorded,
  });
  const armed =
    id.trim() !== "" &&
    name.trim() !== "" &&
    Number.isInteger(slots) &&
    slots > 0 &&
    !action.pending;
  const submit = () => {
    if (armed) action.trigger();
  };

  return (
    <ProjectDialog name="add" pending={action.pending} onClose={onClose} initial={first}>
      <h2 id="add-dialog-title">Add project</h2>
      <p id="add-dialog-body">
        Registers a project on this host, as <code>tiller project add</code> does. Point it at a
        checkout with <code>tiller config set checkouts.&lt;id&gt;</code> before its first builder.
      </p>
      <form
        className="padd-form"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <div className="padd-fields">
          <label htmlFor={`${fields}-id`}>Id</label>
          <input
            ref={first}
            id={`${fields}-id`}
            className="note-in mono"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            placeholder="my-app"
            value={id}
            disabled={action.pending}
            onChange={(event) => setId(event.target.value)}
          />
          <label htmlFor={`${fields}-name`}>Name</label>
          <input
            id={`${fields}-name`}
            className="note-in"
            autoComplete="off"
            placeholder="My app"
            value={name}
            disabled={action.pending}
            onChange={(event) => setName(event.target.value)}
          />
          <label htmlFor={`${fields}-repo`}>Repo</label>
          <input
            id={`${fields}-repo`}
            className="note-in mono"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            placeholder="owner/name, optional"
            value={repo}
            disabled={action.pending}
            onChange={(event) => setRepo(event.target.value)}
          />
          <label htmlFor={`${fields}-delivery`}>Delivery</label>
          <select
            id={`${fields}-delivery`}
            className="ca-select padd-select"
            value={delivery}
            disabled={action.pending}
            onChange={(event) => setDelivery(event.target.value)}
          >
            {DELIVERIES.map((mode) => (
              <option key={mode} value={mode}>
                {mode}
              </option>
            ))}
          </select>
          <label htmlFor={`${fields}-capacity`}>Builder slots</label>
          <input
            id={`${fields}-capacity`}
            className="note-in padd-slots"
            type="number"
            inputMode="numeric"
            min={1}
            step={1}
            value={capacity}
            disabled={action.pending}
            onChange={(event) => setCapacity(event.target.value)}
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
          <button type="submit" className="btn primary" disabled={!armed}>
            {action.pending ? "Adding..." : "Add project"}
          </button>
        </div>
      </form>
    </ProjectDialog>
  );
}

// The "+" in the sidebar's Projects header.
export function AddProject() {
  const [open, setOpen] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => {
    setOpen(false);
    opener.current?.focus();
  }, []);
  return (
    <>
      <button
        ref={opener}
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
