import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";

import type { Boundary, HostFields } from "../../lib/api-types";
import { hostConfigQuery, hostFieldsQuery, machinesQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { WarnIcon } from "../board/icons";
import { MinusIcon, PlusIcon } from "../shell/icons";
import { readFailure } from "../shell/project-dialog";
import { Modal } from "../ui/modal";
import { Segmented } from "../ui/segmented";
import { CardLine, HostCardSection } from "./host-card";
// The inputs, the refusal and the dialog, as the project forms use them.
import "../attention/inbox.css";
import "../shell/project-stage.css";

const MB_PER_GB = 1024;

const BOUNDARIES: { value: Boundary; label: string }[] = [
  { value: "personal", label: "Personal" },
  { value: "work", label: "Work" },
];

// The values as the owner types them. A floor is GB text: empty means no
// floor.
interface Draft {
  lanes: number;
  ram: string;
  disk: string;
  label: string;
  boundary: Boundary;
}

type Field = keyof Draft;

const LABELS: Record<Field, string> = {
  lanes: "Lanes",
  ram: "RAM floor",
  disk: "Disk floor",
  label: "Label",
  boundary: "Boundary",
};

const CAPACITY: Field[] = ["lanes", "ram", "disk"];
const IDENTITY: Field[] = ["label", "boundary"];

// A host label, as `tiller peer add` and `tiller host edit` check it.
const LABEL = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;
// Whole GB, or GB to one decimal.
const GB = /^\d+(\.\d)?$/;

// MB as GB text: whole GB as is, else to one decimal.
function gbText(mb: number | null): string {
  if (mb === null) return "";
  const gb = mb / MB_PER_GB;
  return String(Number.isInteger(gb) ? gb : Math.round(gb * 10) / 10);
}

function mbOf(text: string): number | null {
  const gb = text.trim();
  return gb === "" ? null : Math.round(Number(gb) * MB_PER_GB);
}

const validFloor = (text: string) => text.trim() === "" || GB.test(text.trim());

function draftOf(fields: HostFields): Draft {
  return {
    lanes: fields.lane_capacity ?? 1,
    ram: gbText(fields.ram_floor_mb),
    disk: gbText(fields.disk_floor_mb),
    label: fields.label,
    boundary: fields.boundary ?? "personal",
  };
}

// A field as it would be stored, so "8.0" and "8" are the same floor.
function stored(draft: Draft, field: Field): string {
  switch (field) {
    case "lanes":
      return String(draft.lanes);
    case "ram":
    case "disk": {
      const text = draft[field].trim();
      return GB.test(text) ? String(Number(text)) : text;
    }
    default:
      return draft[field].trim();
  }
}

function changedFields(from: Draft, to: Draft, fields: Field[]): Field[] {
  return fields.filter((field) => stored(from, field) !== stored(to, field));
}

// The PATCH body for what the owner changed. An empty floor clears it.
function editOf(base: Draft, draft: Draft, fields: Field[]): Record<string, unknown> {
  const changed = new Set(changedFields(base, draft, fields));
  const body: Record<string, unknown> = {};
  if (changed.has("lanes")) body.lane_capacity = draft.lanes;
  if (changed.has("ram")) body.ram_floor_mb = mbOf(draft.ram);
  if (changed.has("disk")) body.disk_floor_mb = mbOf(draft.disk);
  if (changed.has("label")) body.label = draft.label.trim();
  if (changed.has("boundary")) body.boundary = draft.boundary;
  return body;
}

// "Lanes is now 4", for the conflict message.
function nowLine(field: Field, fresh: Draft): string {
  const value = stored(fresh, field);
  if (value === "") return `${LABELS[field]} is now empty.`;
  const unit = field === "ram" || field === "disk" ? " GB" : "";
  return `${LABELS[field]} is now ${value}${unit}.`;
}

// "21 GB", rounded down: what is free, never more.
const gbFree = (mb: number) => `${Math.floor(mb / MB_PER_GB)} GB`;

// "Now: 3 of 5 lanes busy, 21 GB RAM free, 140 GB disk free".
function nowText(fields: HostFields): string {
  const lanes =
    fields.lane_capacity === null
      ? `${fields.lanes_busy} lanes busy`
      : `${fields.lanes_busy} of ${fields.lane_capacity} lanes busy`;
  const parts = [lanes];
  if (fields.ram_free_mb !== null) parts.push(`${gbFree(fields.ram_free_mb)} RAM free`);
  if (fields.disk_free_mb !== null) parts.push(`${gbFree(fields.disk_free_mb)} disk free`);
  return `Now: ${parts.join(", ")}`;
}

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

// One section's form over this host's fields: a draft of its own fields,
// saved against the `rev` it was read at. A newer read moves the form while
// the owner has nothing unsaved, and moves only the `rev` under unsaved
// changes when none of this section's fields moved (a save in the other
// section). When one did, the save is refused, says which field moved and
// offers the new values.
function useHostForm(fields: HostFields, own: Field[], onReadAgain: () => void) {
  const queryClient = useQueryClient();
  const [base, setBase] = useState(fields);
  const baseDraft = draftOf(base);
  const [draft, setDraft] = useState(baseDraft);
  const set = <K extends Field>(field: K, value: Draft[K]) =>
    setDraft((current) => ({ ...current, [field]: value }));

  const edit = editOf(baseDraft, draft, own);
  const changed = Object.keys(edit).length > 0;
  const editKey = JSON.stringify(edit);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<unknown>("PATCH", "/api/v1/host", {
        ...(JSON.parse(editKey) as Record<string, unknown>),
        rev: base.rev,
        idempotency_key,
      }),
    [base.rev, editKey],
  );
  const recorded = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: hostFieldsQuery().queryKey });
    void queryClient.invalidateQueries({ queryKey: machinesQuery().queryKey });
    // The host keys below show the boundary too.
    void queryClient.invalidateQueries({ queryKey: hostConfigQuery().queryKey });
  }, [queryClient]);
  // The values and the `rev` are the click: a retry of the same save reuses
  // its key, and any other save is a new one.
  const action = useAction({
    binding: `host-edit:${base.id}:${base.rev}:${editKey}`,
    send,
    onRecorded: recorded,
  });
  const { error } = action;
  const [handled, setHandled] = useState<typeof error>(null);
  const stale = error?.status === 409 && error !== handled;
  useEffect(() => {
    if (stale) onReadAgain();
  }, [stale, onReadAgain]);

  const freshDraft = draftOf(fields);
  if (fields.rev !== base.rev) {
    const untouched = changedFields(baseDraft, draft, own).length === 0;
    const landed = changedFields(freshDraft, draft, own).length === 0;
    const movedOwn = changedFields(baseDraft, freshDraft, own).length > 0;
    if (untouched || landed || !movedOwn) {
      setBase(fields);
      if (untouched || landed) setDraft(freshDraft);
      if (stale) setHandled(error);
    }
  }
  // The fields of this section that moved since the form's `rev`, once a
  // newer read is in.
  const moved = fields.rev !== base.rev ? changedFields(baseDraft, freshDraft, own) : null;

  // The new values: each field that moved takes the stored value, and the
  // owner's other changes stay.
  const load = () => {
    setDraft((current) => {
      const next = { ...current };
      for (const field of moved ?? []) (next as Record<Field, unknown>)[field] = freshDraft[field];
      return next;
    });
    setBase(fields);
    setHandled(error);
  };
  const reset = () => {
    setDraft(baseDraft);
    setHandled(error);
  };

  const conflict = stale
    ? moved === null
      ? `Not saved: ${base.label} changed after you opened it.`
      : `Not saved: ${base.label} changed after you opened it. ${moved.map((field) => nowLine(field, freshDraft)).join(" ")} Load the new values, then save again. Your changes to other fields stay.`
    : null;
  const failure = !stale && error && error !== handled ? error.message.replaceAll("`", "") : null;
  return {
    draft,
    set,
    edit,
    changed,
    action,
    stale,
    ready: moved !== null,
    conflict,
    failure,
    load,
    reset,
  };
}

type HostForm = ReturnType<typeof useHostForm>;

// The foot of a section: its note, then Reset and Save, Save on the right.
// Load new values takes Save's place after a conflict.
function Foot({
  form,
  valid,
  owner,
  note,
  onSave,
}: {
  form: HostForm;
  valid: boolean;
  owner: boolean;
  note?: string;
  onSave: () => void;
}) {
  const { action, changed, stale, ready } = form;
  return (
    <>
      {form.conflict || form.failure ? (
        <p className="refusal set-hrefusal" role="alert">
          {form.conflict ?? form.failure}
        </p>
      ) : null}
      <div className="set-hfoot">
        {note ? <span className="set-hnote">{note}</span> : null}
        {owner ? (
          <>
            <button
              type="button"
              className="btn"
              disabled={!changed || action.pending}
              onClick={form.reset}
            >
              Reset
            </button>
            {stale ? (
              <button type="button" className="btn primary" disabled={!ready} onClick={form.load}>
                Load new values
              </button>
            ) : (
              <button
                type="button"
                className="btn primary"
                disabled={!changed || !valid || action.pending || action.recorded}
                aria-busy={action.pending || undefined}
                onClick={onSave}
              >
                {action.pending ? "Saving..." : "Save"}
              </button>
            )}
          </>
        ) : (
          <span className="set-line-r">Owner only</span>
        )}
      </div>
    </>
  );
}

function Floor({
  id,
  label,
  value,
  disabled,
  onChange,
}: {
  id: string;
  label: string;
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  const bad = !validFloor(value);
  return (
    <div className="pa-f">
      <label className="pa-l" htmlFor={id}>
        {label}
      </label>
      <span className="pa-unit">
        <input
          id={id}
          className="note-in"
          inputMode="decimal"
          autoComplete="off"
          placeholder="None"
          value={value}
          aria-invalid={bad ? true : undefined}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        />
        <span aria-hidden="true">GB</span>
      </span>
    </div>
  );
}

// How many agents this host runs at once and the free RAM and disk it keeps
// (`tiller host edit --lane-capacity --ram-floor-mb --disk-floor-mb`).
function Capacity({
  fields,
  owner,
  onReadAgain,
}: {
  fields: HostFields;
  owner: boolean;
  onReadAgain: () => void;
}) {
  const ids = useId();
  const form = useHostForm(fields, CAPACITY, onReadAgain);
  const { draft, set, action } = form;
  const busy = action.pending || !owner;
  const valid = validFloor(draft.ram) && validFloor(draft.disk);
  const running = fields.lanes_busy;
  // Agents past the new lane count keep running; a new one waits until
  // enough of them end.
  const over = draft.lanes < running ? running - draft.lanes + 1 : 0;
  return (
    <HostCardSection title="Capacity" meta={nowText(fields)}>
      <div className="set-hfields">
        <div className="pa-f">
          <span className="pa-l" id={`${ids}-lanes`}>
            Lanes
          </span>
          <div className="pa-num" role="group" aria-labelledby={`${ids}-lanes`}>
            <button
              type="button"
              aria-label="One lane fewer"
              disabled={busy || draft.lanes <= 1}
              onClick={() => set("lanes", Math.max(1, draft.lanes - 1))}
            >
              <MinusIcon />
            </button>
            <output aria-live="polite">{draft.lanes}</output>
            <button
              type="button"
              aria-label="One lane more"
              disabled={busy}
              onClick={() => set("lanes", draft.lanes + 1)}
            >
              <PlusIcon />
            </button>
          </div>
        </div>
        <Floor
          id={`${ids}-ram`}
          label="RAM floor"
          value={draft.ram}
          disabled={busy}
          onChange={(value) => set("ram", value)}
        />
        <Floor
          id={`${ids}-disk`}
          label="Disk floor"
          value={draft.disk}
          disabled={busy}
          onChange={(value) => set("disk", value)}
        />
      </div>
      {over > 0 ? (
        <p className="set-receipt warn" data-testid="lanes-over">
          <WarnIcon />
          <span>
            <b>
              {running} agents run on {fields.label} now.
            </b>{" "}
            They keep running. No new agent starts until {over} of them end.
          </span>
        </p>
      ) : null}
      <Foot
        form={form}
        valid={valid}
        owner={owner}
        note="No new agent starts while free RAM or disk is under its floor."
        onSave={action.trigger}
      />
    </HostCardSection>
  );
}

// The name peers know this host by, and the data boundary its memory,
// backups and exports carry (`tiller host edit --label --boundary`).
function Identity({
  fields,
  owner,
  peers,
  onReadAgain,
}: {
  fields: HostFields;
  owner: boolean;
  peers: string[];
  onReadAgain: () => void;
}) {
  const ids = useId();
  const form = useHostForm(fields, IDENTITY, onReadAgain);
  const { draft, set, action, edit } = form;
  const [confirming, setConfirming] = useState(false);
  const busy = action.pending || !owner;
  const labelBad = !LABEL.test(draft.label.trim());
  const paired = peers.length > 0;
  const recordedBy = fields.boundary_recorded_by;
  const kept = fields.boundary ?? "personal";
  const save = () => (edit.boundary === undefined ? action.trigger() : setConfirming(true));
  const keep = useRef<HTMLButtonElement>(null);
  const labelHelp = paired
    ? `${peers.join(", ")} ${peers.length === 1 ? "knows" : "know"} this host by this label. Unpair first to rename it.`
    : labelBad
      ? "A letter or digit, then letters, digits, _ or -. At most 64."
      : "Peers and the board know this host by it.";
  const boundaryHelp = recordedBy
    ? `${capital(recordedBy)} already records the ${kept} boundary, so it stays.`
    : "Set at install. It can change until memory, a backup or an export records it.";
  return (
    <HostCardSection title="Label and boundary">
      <div className="set-hfields set-hfields-2">
        <div className="pa-f">
          <label className="pa-l" htmlFor={`${ids}-label`}>
            Label
          </label>
          <input
            id={`${ids}-label`}
            className="note-in mono"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            value={draft.label}
            aria-invalid={labelBad && !paired ? true : undefined}
            aria-describedby={`${ids}-label-help`}
            disabled={busy || paired}
            onChange={(event) => set("label", event.target.value)}
          />
          <p
            className="pa-help"
            id={`${ids}-label-help`}
            data-problem={labelBad && !paired ? true : undefined}
          >
            {labelHelp}
          </p>
        </div>
        <div className="pa-f">
          <span className="pa-l" id={`${ids}-boundary`}>
            Boundary
          </span>
          <Segmented
            className="set-hseg"
            labelledBy={`${ids}-boundary`}
            options={BOUNDARIES}
            value={draft.boundary}
            disabled={busy || recordedBy !== null}
            onChange={(value) => set("boundary", value)}
          />
          <p className="pa-help">{boundaryHelp}</p>
        </div>
      </div>
      <Foot form={form} valid={!labelBad} owner={owner} onSave={save} />
      {confirming ? (
        <Modal
          className="dialog"
          labelledBy={`${ids}-confirm-title`}
          describedBy={`${ids}-confirm-body`}
          onClose={() => setConfirming(false)}
          initialFocus={keep}
        >
          <h2 id={`${ids}-confirm-title`}>Change the boundary to {draft.boundary}?</h2>
          <p id={`${ids}-confirm-body`}>
            Memory, backups and exports carry the boundary, and a restore or an import refuses one
            from the other boundary. Nothing records the {kept} boundary yet. Once memory, a backup
            or an export records {draft.boundary}, it stays.
          </p>
          <div className="dialog-actions">
            <button ref={keep} type="button" className="btn" onClick={() => setConfirming(false)}>
              Keep {kept}
            </button>
            <button
              type="button"
              className="btn primary"
              onClick={() => {
                // The footer Save shows the save; a refusal shows under it.
                setConfirming(false);
                action.trigger();
              }}
            >
              {`Change to ${draft.boundary}`}
            </button>
          </div>
        </Modal>
      ) : null}
    </HostCardSection>
  );
}

// This host's own settings in its card, under the checks: the ones
// `tiller host edit` sets, read fresh each time the card opens.
export function HostEdit({ owner, peers }: { owner: boolean; peers: string[] }) {
  const read = useQuery(hostFieldsQuery());
  const { refetch } = read;
  const readAgain = useCallback(() => void refetch(), [refetch]);
  if (!read.data) {
    const failed = readFailure(read.error, "This host could not be read.");
    return (
      <Waiting title="Capacity">
        <CardLine
          testid="host-fields-unread"
          tone={failed ? "amber" : null}
          title={failed ? "Not read" : "Reading this host..."}
          detail={failed ? [failed, " Run tiller host edit in a shell on this host."] : undefined}
        />
      </Waiting>
    );
  }
  return (
    <>
      <Capacity fields={read.data} owner={owner} onReadAgain={readAgain} />
      <Identity fields={read.data} owner={owner} peers={peers} onReadAgain={readAgain} />
    </>
  );
}

function Waiting({ title, children }: { title: string; children: ReactNode }) {
  return (
    <HostCardSection title={title}>
      <ul className="set-lines">{children}</ul>
    </HostCardSection>
  );
}
