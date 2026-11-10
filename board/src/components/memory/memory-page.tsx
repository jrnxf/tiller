import { useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useBlocker } from "@tanstack/react-router";
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, MouseEvent, ReactNode } from "react";

import type {
  MemoryDocSummary,
  MemoryDocument,
  MemoryLine,
  MemoryProvenance,
  MemoryResponse,
  MemoryRevision,
  MemorySection,
  MemorySuggestion,
} from "../../lib/api-types";
import { hostName, isPeerRow } from "../../lib/peers";
import { prefsStore, visibleProjects } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import {
  API_KEY_PREFIX,
  ReadError,
  memoryQuery,
  projectsQuery,
  revisionQuery,
} from "../../lib/queries";
import { useStore } from "../../lib/store";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { WarnIcon } from "../board/icons";
import { showToast } from "../board/toast";
import { lisbonDay, lisbonShortDay } from "../health/lisbon";
import { CheckIcon, LayersIcon } from "../shell/icons";
import { ProjectDialog } from "../shell/project-dialog";
import { Section } from "../states/section";
import { ClockIcon, EditIcon, LinesIcon, PlusIcon, SparkIcon, TrashIcon } from "./icons";
import { inlineDiff } from "./inline-diff";
import "../shell/project-stage.css";
import "./memory.css";
import { useMachineRows, useNamedProjects } from "../board/peer-state";

const FLEET = "fleet";

// The shared document lives on the `fleet` project, named for this host as
// the board names that project everywhere ("host / personal-mac").
function useDocName(): (doc: { project_id: string; name: string }) => string {
  const local = useMachineRows().find((machine) => machine.kind === "local")?.label ?? null;
  return (doc) => (doc.project_id === FLEET ? hostName(local) : doc.name);
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

// "Wed 4 Nov" and "Wed 4 Nov, 17:48", in Lisbon time.
const day = (iso: string) => lisbonShortDay(iso);
const when = (iso: string) => `${lisbonShortDay(iso)}, ${lisbonDay(iso).time}`;

const oneLine = (text: string | null | undefined) => (text ?? "").replace(/\s+/g, " ").trim();

// A free-text reason as a sentence of its own: a capital letter first and a
// full stop last, so "seen twice" reads "Seen twice.".
function sentence(text: string): string {
  const trimmed = oneLine(text);
  if (trimmed === "") return "";
  const capital = `${trimmed.charAt(0).toUpperCase()}${trimmed.slice(1)}`;
  return /[.!?]$/.test(capital) ? capital : `${capital}.`;
}

// The note a landed change shows, so the page says it worked. `scope` names
// the control that sent the change, so a refusal shows beside it.
type Write = (
  path: string,
  body: Record<string, unknown>,
  landed: string,
  scope: string,
) => Promise<boolean>;

// A refused change and the control it came from.
interface Refusal {
  scope: string;
  error: ActionError;
}

// Where a refusal shows: in the open editor, on a suggestion's card, or on a
// line's row.
const EDITOR = "editor";
const proposalScope = (id: string) => `proposal:${id}`;
const lineScope = (section: string, nth: number, text: string) =>
  JSON.stringify([section, nth, text]);

// Every change names the blob the page showed, so a click that lands twice, or
// one made against a text that moved on, is refused instead of applied again.
// Each act therefore gets its own idempotency key. Landed or refused, the
// document is read again: the change shows, or the text it was refused against.
function useMemoryWrite() {
  const queryClient = useQueryClient();
  const inFlight = useRef(false);
  const [pending, setPending] = useState(false);
  const [refusal, setRefusal] = useState<Refusal | null>(null);

  const write = useCallback<Write>(
    async (path, body, note, scope) => {
      if (inFlight.current) return false;
      inFlight.current = true;
      setPending(true);
      setRefusal(null);
      let landed = false;
      try {
        await sendMutation("POST", path, { idempotency_key: ulid(), ...body });
        landed = true;
      } catch (caught) {
        setRefusal({
          scope,
          error:
            caught instanceof ActionError
              ? caught
              : new ActionError(0, "internal", "Something went wrong, so this was not saved."),
        });
      }
      await queryClient.invalidateQueries({
        queryKey: API_KEY_PREFIX,
        predicate: (query) => String(query.queryKey[1] ?? "").startsWith("/api/v1/memory"),
      });
      inFlight.current = false;
      setPending(false);
      if (landed) showToast("ok", note);
      return landed;
    },
    [queryClient],
  );
  const clearRefusal = useCallback(() => setRefusal(null), []);

  return { write, pending, refusal, clearRefusal };
}

function colorOf(projects: { id: string }[], id: string): string {
  const index = projects.findIndex((project) => project.id === id);
  return index === -1 ? "var(--fg-3)" : projectColor(index);
}

// The argument the memory read takes: the shared document is the default one,
// so it is never asked for by name (one cache entry, one request).
const scopeOf = (scope: string) => (scope === FLEET ? undefined : scope);

const isMissing = (error: unknown) => error instanceof ReadError && error.status === 404;

// One document's answer, or `null` when the daemon has no such document, which
// the page says plainly instead of failing. Any other failure goes to the
// section's note, with its retry. `undefined` means the read is still going.
function useDocAnswer(scope: string): MemoryResponse | null | undefined {
  const query = useQuery({
    ...memoryQuery(scopeOf(scope)),
    throwOnError: (error) => !isMissing(error),
  });
  if (query.data) return query.data;
  return isMissing(query.error) ? null : undefined;
}

// One tab per memory document, the shared one first. A tab shows the
// suggestions waiting on it, or its line count when there are none.
function TabStrip({ docs: listed, current }: { docs: MemoryDocSummary[]; current: string }) {
  const docName = useDocName();
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const prefs = useStore(prefsStore);
  const local = projects.projects.filter((project) => !isPeerRow(project));
  const shown = new Set(visibleProjects(prefs, local).map((project) => project.id));
  const docs = listed.filter(
    (doc) => doc.project_id === FLEET || doc.project_id === current || shown.has(doc.project_id),
  );
  return (
    <nav className="m2-tabs" aria-label="Memory documents">
      {docs.map((doc) => (
        <Link
          key={doc.project_id}
          to="/memory"
          search={doc.project_id === FLEET ? {} : { doc: doc.project_id }}
          // The shared tab's empty search is part of every other tab's, so
          // only an exact match counts as the open one.
          activeOptions={{ exact: true }}
          className="m2-tab"
          aria-current={doc.project_id === current ? "page" : undefined}
          style={{ "--pc": colorOf(local, doc.project_id) } as CSSProperties}
        >
          {doc.project_id === FLEET ? <LayersIcon /> : <span className="pdot" />}
          {docName(doc)}
          {doc.suggestions > 0 ? (
            <span className="s" title={`${plural(doc.suggestions, "suggestion")} waiting`}>
              {doc.suggestions}
            </span>
          ) : (
            <span className="n" title={plural(doc.lines, "line")}>
              {doc.lines}
            </span>
          )}
        </Link>
      ))}
    </nav>
  );
}

// An unknown document has no answer to list its siblings from, so the shared
// document's answer does.
function SharedTabs({ current }: { current: string }) {
  const { data } = useSuspenseQuery(memoryQuery());
  return <TabStrip docs={data.docs} current={current} />;
}

function DocTabs({ current }: { current: string }) {
  const answer = useDocAnswer(current);
  if (answer === undefined) return null;
  if (answer === null) return <SharedTabs current={current} />;
  return <TabStrip docs={answer.docs} current={current} />;
}

// Who last changed a line, as one unit that never wraps: a card in its
// project's colour, you, Tiller's own curator, or the file's history.
function Who({
  provenance,
  projects,
}: {
  provenance: MemoryProvenance;
  projects: { id: string }[];
}) {
  let mark: ReactNode;
  let name: string;
  if (provenance.card) {
    const project = provenance.card.replace(/-\d+$/, "");
    mark = (
      <span className="m2av card" style={{ "--pc": colorOf(projects, project) } as CSSProperties} />
    );
    name = provenance.card;
  } else if (provenance.note === "you") {
    mark = <span className="m2av you">Y</span>;
    name = "you";
  } else if (provenance.note === "carried over") {
    mark = (
      <span className="m2av">
        <ClockIcon />
      </span>
    );
    name = "carried over";
  } else if (provenance.note === "outside Tiller") {
    mark = (
      <span className="m2av">
        <EditIcon />
      </span>
    );
    name = "outside Tiller";
  } else {
    mark = (
      <span className="m2av bot">
        <SparkIcon />
      </span>
    );
    name = provenance.note ?? "unknown";
  }
  return (
    <span className="who" title={name}>
      {mark}
      <span className="nm">{name}</span>
    </span>
  );
}

// What an editor is doing. `edit` rewrites its line and `add` puts a new line
// at the end of a section. `changed` is a line that moved or changed
// elsewhere under the draft, and `gone` one that was removed: both keep the
// draft and wait for a choice on a button.
type EditorMode = "edit" | "add" | "changed" | "gone";

const SAVE: Record<EditorMode, string> = {
  edit: "Save",
  add: "Add line",
  changed: "Save over",
  gone: "Add as new line",
};

// A refusal or a note shown beside the control it is about, and announced
// where it appears.
function Inline({ error }: { error: ActionError }) {
  return (
    <p className="refusal inline" role="alert">
      {error.message}
    </p>
  );
}

// The editor a line opens in place, and Add a line opens at the end of its
// section. Enter saves and Esc discards; a blank line is never saved. After a
// change under the draft, Enter does not save: that choice is made on a button.
function LineEditor({
  value,
  onChange,
  label,
  meta,
  mode,
  notice,
  refusal,
  locked,
  onSave,
  onDiscard,
  onRemove,
}: {
  value: string;
  onChange: (value: string) => void;
  label: string;
  meta: ReactNode;
  mode: EditorMode;
  notice: ReactNode;
  refusal: ActionError | null;
  locked: boolean;
  onSave: () => void;
  onDiscard: () => void;
  onRemove?: () => void;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const plain = mode === "edit" || mode === "add";
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);
  // A send disables the buttons, and the one that held focus loses it. When it
  // ends with the editor still open (the change was refused), the draft takes
  // focus back.
  const wasLocked = useRef(locked);
  useEffect(() => {
    const active = document.activeElement;
    const lost =
      active === null ||
      active === document.body ||
      (active instanceof HTMLButtonElement && active.disabled);
    if (wasLocked.current && !locked && lost) ref.current?.focus({ preventScroll: true });
    wasLocked.current = locked;
  }, [locked]);
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // An input method's Enter picks a candidate; it does not save.
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Escape") {
      event.preventDefault();
      onDiscard();
    } else if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      if (plain && !locked) onSave();
    }
  };
  return (
    <div className="ed">
      <textarea
        ref={ref}
        rows={1}
        placeholder="Write a line..."
        aria-label={label}
        spellCheck={false}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={onKeyDown}
      />
      {notice}
      {refusal ? <Inline error={refusal} /> : null}
      <div className="ed-f">
        <span className="ed-meta">{meta}</span>
        <span className="ed-keys" aria-hidden="true">
          {plain ? (
            <>
              <kbd>↵</kbd>save
              <span className="gap" />
            </>
          ) : null}
          <kbd>Esc</kbd>
          {plain ? "cancel" : "discard"}
        </span>
        <span className="ed-b">
          {onRemove ? (
            <button type="button" className="btn ed-rm" disabled={locked} onClick={onRemove}>
              <TrashIcon />
              Remove
            </button>
          ) : null}
          <button type="button" className="btn grow" onClick={onDiscard}>
            {plain ? "Cancel" : "Discard"}
          </button>
          <button type="button" className="btn primary" disabled={locked} onClick={onSave}>
            {SAVE[mode]}
          </button>
        </span>
      </div>
    </div>
  );
}

// The editor's quiet footer for a line: who wrote it, when, and its source,
// the file at that revision with its approval and the sessions that loaded it.
function SourceMeta({ line, projects }: { line: MemoryLine; projects: { id: string }[] }) {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const provenance = line.provenance;
  if (!provenance) return <span>Not yet recorded</span>;
  return (
    <>
      <Who provenance={provenance} projects={projects} />
      <span className="mk-sep">·</span>
      <span>{day(provenance.at)}</span>
      {provenance.revision_id ? (
        <>
          <span className="mk-sep">·</span>
          <button
            ref={button}
            type="button"
            className="ed-src"
            title="The file at this revision, its approval and the sessions that loaded it"
            onClick={() => setOpen(true)}
          >
            Source
          </button>
        </>
      ) : null}
      {open ? (
        <SourceDialog
          id={provenance.revision_id}
          onClose={() => {
            setOpen(false);
            requestAnimationFrame(() => button.current?.focus());
          }}
        />
      ) : null}
    </>
  );
}

// The line's buttons act on their own; a click anywhere else on the line edits it.
const act = (run: () => void) => (event: MouseEvent) => {
  event.stopPropagation();
  run();
};

function LineRow({
  line,
  projects,
  editing,
  locked,
  focus,
  onEdit,
  onRemove,
}: {
  line: MemoryLine;
  projects: { id: string }[];
  editing: boolean;
  locked: boolean;
  // Names this row for the focus that comes back to it (see `Focus`).
  focus: string;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const provenance = line.provenance;
  const who = provenance ? (
    <Who provenance={provenance} projects={projects} />
  ) : (
    <span className="who" />
  );
  const changed = provenance ? day(provenance.at) : "";
  return (
    <div
      className="a-ln"
      hidden={editing}
      data-locked={locked || undefined}
      onClick={locked ? undefined : onEdit}
    >
      <span className="bul" />
      <div className="tx">{line.text}</div>
      {who}
      <span className="day">{changed}</span>
      <div className="meta-m">
        {who}
        {changed ? (
          <>
            <span className="mk-sep">·</span>
            <span className="day">{changed}</span>
          </>
        ) : null}
      </div>
      <div className="acts">
        <button
          type="button"
          className="ib"
          title="Edit this line"
          aria-label="Edit this line"
          data-focus={focus}
          disabled={locked}
          onClick={act(onEdit)}
        >
          <EditIcon />
        </button>
        <button
          type="button"
          className="ib danger"
          title="Remove this line"
          aria-label="Remove this line"
          disabled={locked}
          onClick={act(onRemove)}
        >
          <TrashIcon />
        </button>
      </div>
    </div>
  );
}

function budgetText(budget: Record<string, number> | null): string {
  if (!budget) return "";
  const roles = Object.entries(budget).filter(([, tokens]) => typeof tokens === "number");
  if (roles.length === 0) return "";
  const parts = roles.map(([role, tokens]) => `${role} ${tokens.toLocaleString("en-GB")}`);
  return `Startup prompt after: ${parts.join(", ")} tokens.`;
}

// A suggestion sits right where it would land: a rewrite or a removal under
// the line it changes, a new line at the end of its section.
function SuggestionCard({
  suggestion,
  line,
  section,
  locked,
  refusal,
  onDecide,
}: {
  suggestion: MemorySuggestion;
  line: MemoryLine | null;
  section: string;
  locked: boolean;
  // Why Keep or Discard was refused, beside the buttons that were pressed.
  refusal: ActionError | null;
  onDecide: (suggestion: MemorySuggestion, decision: "keep" | "discard") => void;
}) {
  // The line it names: the row it sits under, or the text the view sends when
  // it is listed apart from that row.
  const current = line?.text ?? suggestion.target;
  const where = line ? "of the line above" : "of a line in this section";
  const { gate } = suggestion;
  const gateId = `gate-${suggestion.proposal_id}`;
  const gateReason = oneLine(gate.reason).replace(/[.!?]+$/, "");
  let kind: string;
  let head: string;
  let body: ReactNode;
  if (suggestion.op === "add") {
    [kind, head] = ["Suggested line", `for ${section}`];
    body = <ins className="ins">{suggestion.text}</ins>;
  } else if (suggestion.op === "rewrite") {
    [kind, head] = ["Suggested rewrite", where];
    body =
      current === null ? (
        <ins className="ins">{suggestion.text}</ins>
      ) : (
        inlineDiff(current, suggestion.text ?? current).map((part, i) => {
          const gap = i > 0 ? " " : "";
          if (part.kind === "del")
            return [
              gap,
              <del key={i} className="del">
                {part.text}
              </del>,
            ];
          if (part.kind === "ins")
            return [
              gap,
              <ins key={i} className="ins">
                {part.text}
              </ins>,
            ];
          return `${gap}${part.text}`;
        })
      );
  } else {
    [kind, head] = ["Suggested removal", where];
    body =
      current === null ? (
        "The line it removes is no longer in the file."
      ) : (
        <del className="del">{current}</del>
      );
  }
  const from = suggestion.card ? ` from ${suggestion.card}` : "";
  const reason = sentence(suggestion.why ?? "");
  const budget = budgetText(suggestion.token_budget_after);
  return (
    <div
      className="a-sug"
      id={`sug-${suggestion.proposal_id}`}
      tabIndex={-1}
      role="group"
      aria-label={`Suggestion from ${suggestion.by}`}
      data-op={suggestion.op}
    >
      <div className="a-sug-k">
        <b>
          <SparkIcon />
          {kind}
        </b>
        {head}
      </div>
      <div className="tx">{body}</div>
      <div className="why">
        Suggested by {suggestion.by}
        {from}, {when(suggestion.at)}.{reason ? ` ${reason}` : ""}
        {budget ? <span className="bud"> {budget}</span> : null}
      </div>
      {gate.ok ? null : (
        <div className="gate" id={gateId}>
          <WarnIcon />
          <span>
            Keep waits for the holdout gate{gateReason ? `: ${gateReason}` : ""}.
            {gate.hint ? (
              <span className="gate-run">
                {" "}
                Next: <code>{gate.hint}</code>
              </span>
            ) : null}
          </span>
        </div>
      )}
      {refusal ? <Inline error={refusal} /> : null}
      <div className="sug-f">
        {gate.ok ? <span className="note">Nothing changes until you keep it.</span> : null}
        <button
          type="button"
          className="btn"
          disabled={locked}
          onClick={() => onDecide(suggestion, "discard")}
        >
          Discard
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={locked || !gate.ok}
          aria-describedby={gate.ok ? undefined : gateId}
          onClick={() => onDecide(suggestion, "keep")}
        >
          <CheckIcon />
          Keep
        </button>
      </div>
    </div>
  );
}

// Which line an editor was opened on: its text and which copy of that text it
// is in the section (an index shifts when a line above changes), or the
// section's new line (`text` null).
type Open = { section: string; text: string | null; nth: number };

// Each line with which copy of its text it is, counted from the top.
const numbered = (lines: MemoryLine[]) => {
  const seen = new Map<string, number>();
  return lines.map((line) => {
    const nth = seen.get(line.text) ?? 0;
    seen.set(line.text, nth + 1);
    return { line, nth };
  });
};

// The one open editor. The draft lives here, above the lines, so a change of
// editor can ask before it drops one that was changed.
interface EditorState {
  key: number;
  target: Open;
  // The line's stable id, so a rewrite elsewhere still finds it.
  memoryId: string | null;
  // The text the editor opened on.
  initial: string;
  draft: string;
  // While a save is in flight, what the editor showed when it was sent, so
  // the change landing does not flash the "changed elsewhere" notice.
  frozen: Resolved | null;
}

// What the open editor stands on in the document as it is now.
type Resolved =
  | { mode: "add"; section: string }
  | { mode: "gone"; section: string }
  | {
      mode: "edit" | "changed";
      section: string;
      position: number;
      nth: number;
      line: MemoryLine;
      // The line's text differs from the text the editor opened on.
      edited: boolean;
      // The line is in another section than the one the editor opened in.
      moved: boolean;
    };

function resolve(doc: MemoryDocument, editor: EditorState): Resolved | null {
  const { target } = editor;
  if (target.text === null) {
    return doc.sections.some((section) => section.title === target.section)
      ? { mode: "add", section: target.section }
      : null;
  }
  for (const section of doc.sections) {
    for (const [position, { line, nth }] of numbered(section.lines).entries()) {
      const same =
        editor.memoryId !== null
          ? line.memory_id === editor.memoryId
          : section.title === target.section && line.text === target.text && nth === target.nth;
      if (!same) continue;
      const edited = line.text !== editor.initial;
      const moved = section.title !== target.section;
      return {
        mode: edited || moved ? "changed" : "edit",
        section: section.title,
        position,
        nth,
        line,
        edited,
        moved,
      };
    }
  }
  return doc.sections.some((section) => section.title === target.section)
    ? { mode: "gone", section: target.section }
    : null;
}

// Where focus goes when an editor closes or a line is removed: the row at
// `at` in the section (the last one when `at` is past the end), or Add a line.
interface Focus {
  section: string;
  at: number | "add";
  // The blob the page showed when the change was sent. Focus waits for a
  // document that differs, so it lands on the rows as they are after the
  // change. `null` for a close that changed nothing.
  from: string | null;
}

const focusKey = (section: string, at: number | "add") => JSON.stringify([section, at]);

// What the document's sections ask of the page above them.
interface Ctl {
  editor: EditorState | null;
  resolved: Resolved | null;
  locked: boolean;
  refusalFor: (scope: string) => ActionError | null;
  openLine: (section: string, line: MemoryLine, nth: number) => void;
  openAdd: (section: string) => void;
  change: (draft: string) => void;
  save: () => void;
  discard: () => void;
  removeInEditor: () => void;
  removeRow: (section: string, line: MemoryLine, nth: number, position: number) => void;
  decide: (suggestion: MemorySuggestion, decision: "keep" | "discard") => void;
}

// Who a new line is added as.
const adds = (section: string) => (
  <>
    <span>Adds to {section} as</span>
    <span className="who">
      <span className="m2av you">Y</span>
      <span className="nm">you</span>
    </span>
  </>
);

// The open editor, in the state it is in.
function EditorSlot({ ctl, projects }: { ctl: Ctl; projects: { id: string }[] }) {
  const { editor, resolved } = ctl;
  if (!editor || !resolved) return null;
  const { mode } = resolved;
  let meta: ReactNode;
  let notice: ReactNode = null;
  if ("line" in resolved) {
    meta = <SourceMeta line={resolved.line} projects={projects} />;
    if (mode === "changed") {
      notice = (
        <div className="ed-stale" role="alert">
          {resolved.edited ? (
            <>
              This line changed while you were editing. It now reads{" "}
              <span className="was">{resolved.line.text}</span>.
            </>
          ) : (
            <>This line moved to {resolved.section} while you were editing.</>
          )}{" "}
          Save over replaces it with your text. Discard keeps it as it is.
        </div>
      );
    }
  } else {
    meta = adds(resolved.section);
    if (mode === "gone") {
      notice = (
        <div className="ed-stale" role="alert">
          This line was removed while you were editing. Add your text as a new line, or discard it.
        </div>
      );
    }
  }
  return (
    <LineEditor
      key={editor.key}
      value={editor.draft}
      onChange={ctl.change}
      label={mode === "edit" || mode === "changed" ? "Edit this line" : "New line"}
      meta={meta}
      mode={mode}
      notice={notice}
      refusal={ctl.refusalFor(EDITOR)}
      locked={ctl.locked}
      onSave={ctl.save}
      onDiscard={ctl.discard}
      onRemove={mode === "edit" ? ctl.removeInEditor : undefined}
    />
  );
}

function DocSection({
  section,
  projects,
  ctl,
}: {
  section: MemorySection;
  projects: { id: string }[];
  ctl: Ctl;
}) {
  const { resolved } = ctl;
  // A new line, or one whose line was removed, opens its editor at the end.
  const atEnd =
    resolved !== null &&
    (resolved.mode === "add" || resolved.mode === "gone") &&
    resolved.section === section.title;
  const empty = section.lines.length === 0 && section.suggestions.length === 0;

  return (
    <section className="a-sec" aria-label={section.title}>
      <div className="a-sec-h">
        <h2>
          {section.title}
          <span>{plural(section.lines.length, "line")}</span>
        </h2>
        <span className="col" aria-hidden="true">
          From
        </span>
        <span className="col r" aria-hidden="true">
          Changed
        </span>
      </div>
      {numbered(section.lines).map(({ line, nth }, position) => {
        const editing =
          resolved !== null &&
          "line" in resolved &&
          resolved.section === section.title &&
          resolved.position === position;
        const refusal = ctl.refusalFor(lineScope(section.title, nth, line.text));
        return (
          // Keyed by its text, so a landed edit starts the line afresh.
          <div className="a-item" key={`${nth}:${line.text}`}>
            <LineRow
              line={line}
              projects={projects}
              editing={editing}
              locked={ctl.locked}
              focus={focusKey(section.title, position)}
              onEdit={() => ctl.openLine(section.title, line, nth)}
              onRemove={() => ctl.removeRow(section.title, line, nth, position)}
            />
            {refusal ? <Inline error={refusal} /> : null}
            {editing ? (
              <div className="a-edrow">
                <EditorSlot ctl={ctl} projects={projects} />
              </div>
            ) : null}
            {line.suggestion ? (
              <SuggestionCard
                suggestion={line.suggestion}
                line={line}
                section={section.title}
                locked={ctl.locked}
                refusal={ctl.refusalFor(proposalScope(line.suggestion.proposal_id))}
                onDecide={ctl.decide}
              />
            ) : null}
          </div>
        );
      })}
      {section.suggestions.map((suggestion) => (
        <SuggestionCard
          key={suggestion.proposal_id}
          suggestion={suggestion}
          line={null}
          section={section.title}
          locked={ctl.locked}
          refusal={ctl.refusalFor(proposalScope(suggestion.proposal_id))}
          onDecide={ctl.decide}
        />
      ))}
      {empty && !atEnd ? (
        <p className="a-empty">
          Nothing here yet. Agents suggest lines as they learn, or add your own.
        </p>
      ) : null}
      {atEnd ? (
        <div className="a-edrow">
          <EditorSlot ctl={ctl} projects={projects} />
        </div>
      ) : (
        <button
          className="a-add"
          type="button"
          data-focus={focusKey(section.title, "add")}
          disabled={ctl.locked}
          onClick={() => ctl.openAdd(section.title)}
        >
          <PlusIcon />
          Add a line
        </button>
      )}
    </section>
  );
}

const PROVENANCE: Record<string, string> = {
  tiller: "Written through Tiller",
  external: "Edited outside Tiller",
  "legacy-import": "Carried over",
  baseline: "The text Tiller started from",
};

const REVIEW: Record<MemoryRevision["review"], string> = {
  approved: "Approved",
  unreviewed: "Not reviewed",
  revoked: "Revoked",
};

const text = (value: unknown) => (typeof value === "string" ? value : null);

function RevisionDetail({ id }: { id: string }) {
  const { data: revision } = useSuspenseQuery(revisionQuery(id));
  const approvalState = text(revision.approval?.state);
  const decidedBy = text(revision.approval?.decided_by);
  return (
    <div className="rev">
      <h3 className="rev-ti">{revision.path}</h3>
      <dl className="rev-props">
        <dt>Recorded</dt>
        <dd>{when(revision.recorded_at)}</dd>
        <dt>Source</dt>
        <dd>{PROVENANCE[revision.provenance] ?? revision.provenance}</dd>
        <dt>Review</dt>
        <dd>{REVIEW[revision.review] ?? revision.review}</dd>
        {revision.commit_sha ? (
          <>
            <dt>Commit</dt>
            <dd>
              <code>{revision.commit_sha.slice(0, 7)}</code>
            </dd>
          </>
        ) : null}
        {revision.card ? (
          <>
            <dt>Card</dt>
            <dd>{revision.card}</dd>
          </>
        ) : null}
        <dt>Approval</dt>
        <dd>
          {approvalState
            ? `${approvalState[0]?.toUpperCase()}${approvalState.slice(1)}${decidedBy ? `, by ${decidedBy}` : ""}`
            : "None"}
        </dd>
      </dl>
      <h4 className="rev-h">Loaded by</h4>
      {revision.sessions.length === 0 ? (
        <p className="rev-none">No session has loaded this revision yet.</p>
      ) : (
        <ul className="rev-loads">
          {revision.sessions.map((load) => (
            <li key={`${load.session_id}:${load.trigger}:${load.loaded_at}`}>
              <b>{load.card ?? load.role}</b>
              <span>
                {load.card ? `${load.role} in ${load.project_id}` : load.project_id}, {load.trigger}
                , {when(load.loaded_at)}
              </span>
            </li>
          ))}
        </ul>
      )}
      <h4 className="rev-h">File at this revision</h4>
      {revision.text === null ? (
        <p className="rev-none">This revision is no longer in git.</p>
      ) : (
        <pre className="rev-text">{revision.text.trimEnd()}</pre>
      )}
    </div>
  );
}

// A line's source opens over the page, so the document never moves under a
// click.
function SourceDialog({ id, onClose }: { id: string; onClose: () => void }) {
  const close = useRef<HTMLButtonElement>(null);
  return (
    <ProjectDialog
      name="source"
      className="rev-dialog"
      pending={false}
      onClose={onClose}
      initial={close}
    >
      <h2 id="source-dialog-title">Source of this line</h2>
      <p id="source-dialog-body">
        The file at the revision that wrote it, its approval and the sessions that loaded it.
      </p>
      <Section label="This revision">
        <RevisionDetail id={id} />
      </Section>
      <div className="dialog-actions">
        <button ref={close} type="button" className="btn" onClick={onClose}>
          Close
        </button>
      </div>
    </ProjectDialog>
  );
}

// Asks before the draft is dropped, whether for another line or for another
// page. Keeping the draft is the first and safe choice.
function DiscardDialog({
  leaving,
  onKeep,
  onDiscard,
}: {
  leaving: boolean;
  onKeep: () => void;
  onDiscard: () => void;
}) {
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <ProjectDialog name="discard" pending={false} onClose={onKeep} initial={keep}>
      <h2 id="discard-dialog-title">Discard your changes?</h2>
      <p id="discard-dialog-body">
        {leaving
          ? "The line you are editing has changes that are not saved. Leaving this page discards them."
          : "The line you are editing has changes that are not saved. Opening another line discards them."}
      </p>
      <div className="dialog-actions">
        <button ref={keep} type="button" className="btn" onClick={onKeep}>
          Keep editing
        </button>
        <button type="button" className="btn danger" onClick={onDiscard}>
          Discard changes
        </button>
      </div>
    </ProjectDialog>
  );
}

// Whether the control a refusal came from is still on the page, so the
// refusal does not outlive it.
function scopeLive(scope: string, doc: MemoryDocument, editor: EditorState | null): boolean {
  if (scope === EDITOR) return editor !== null;
  return doc.sections.some(
    (section) =>
      section.suggestions.some((each) => proposalScope(each.proposal_id) === scope) ||
      numbered(section.lines).some(
        ({ line, nth }) =>
          (line.suggestion !== null && proposalScope(line.suggestion.proposal_id) === scope) ||
          lineScope(section.title, nth, line.text) === scope,
      ),
  );
}

function Doc({ data }: { data: MemoryResponse }) {
  const docName = useDocName();
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const doc = data.doc;
  const { write, pending, refusal, clearRefusal } = useMemoryWrite();
  const root = useRef<HTMLElement>(null);
  const keys = useRef(0);
  const [editor, setEditor] = useState<EditorState | null>(null);
  // The editor the person asked for while a changed draft was open.
  const [asking, setAsking] = useState<EditorState | null>(null);
  // Where focus goes next, held outside state: it is read by the effect below
  // and never shown. `wake` renders once more so the effect looks again.
  const wanted = useRef<Focus | null>(null);
  const [, wake] = useReducer((n: number) => n + 1, 0);
  const setFocus = (at: Focus) => {
    wanted.current = at;
    wake();
  };
  const locked = doc.dirty || pending;
  const local = projects.projects.filter((project) => !isPeerRow(project));

  const dirty =
    editor !== null &&
    oneLine(editor.draft) !== (editor.target.text === null ? "" : oneLine(editor.initial));
  let resolved = editor ? (editor.frozen ?? resolve(doc, editor)) : null;
  if (editor && editor.frozen === null) {
    if (resolved === null) {
      // Its section is gone, so there is nowhere to show it.
      setEditor(null);
    } else if (!dirty && (resolved.mode === "changed" || resolved.mode === "gone")) {
      // Nothing was typed, so nothing is lost: it follows the line, or closes
      // when the line is gone.
      setEditor(
        resolved.mode === "gone"
          ? null
          : {
              ...editor,
              target: { section: resolved.section, text: resolved.line.text, nth: resolved.nth },
              initial: resolved.line.text,
              draft: resolved.line.text,
            },
      );
      resolved = null;
    }
  }

  // Leaving the page, or going to another document, with a changed draft asks
  // first too.
  const blocker = useBlocker({
    shouldBlockFn: () => true,
    enableBeforeUnload: dirty,
    disabled: !dirty,
    withResolver: true,
  });

  // A refusal goes when the control it came from does.
  useEffect(() => {
    if (refusal && !scopeLive(refusal.scope, doc, editor)) clearRefusal();
  }, [refusal, doc, editor, clearRefusal]);

  // Focus returns to the row or button the editor, or the removed line, left
  // behind, once the page shows the document as the change made it.
  useEffect(() => {
    const focus = wanted.current;
    if (!focus || locked) return;
    if (focus.from !== null && doc.blob === focus.from) return;
    wanted.current = null;
    const active = document.activeElement;
    if (active !== null && active !== document.body) return;
    const section = doc.sections.find((each) => each.title === focus.section);
    let at = focus.at;
    if (section && at !== "add") {
      at = section.lines.length === 0 ? "add" : Math.min(at, section.lines.length - 1);
    }
    const key = focusKey(focus.section, at);
    const target = [...(root.current?.querySelectorAll<HTMLElement>("[data-focus]") ?? [])].find(
      (element) => element.dataset.focus === key,
    );
    target?.focus({ preventScroll: true });
  });

  const edit = (scope: string, section: string, body: Record<string, unknown>, note: string) =>
    write(
      "/api/v1/memory/edits",
      { project: doc.project_id, section, base_blob: doc.blob, ...body },
      note,
      scope,
    );

  const begin = (target: Open, initial: string, memoryId: string | null) => {
    const next: EditorState = {
      key: (keys.current += 1),
      target,
      memoryId,
      initial,
      draft: initial,
      frozen: null,
    };
    if (dirty) {
      setAsking(next);
      return;
    }
    clearRefusal();
    setEditor(next);
  };

  const close = (at: Focus | null) => {
    setEditor(null);
    clearRefusal();
    if (at) setFocus(at);
  };

  const discard = () => {
    if (!resolved) return close(null);
    close({
      section: resolved.section,
      at: "line" in resolved ? resolved.position : "add",
      from: null,
    });
  };

  // Sends the open editor's change. It closes the editor when it lands, and
  // when it is refused leaves the editor open with its draft.
  const send = (section: string, body: Record<string, unknown>, note: string, at: Focus["at"]) => {
    if (!editor) return;
    const { key } = editor;
    const from = doc.blob;
    setEditor({ ...editor, frozen: resolved });
    void edit(EDITOR, section, body, note).then((landed) => {
      if (landed) setFocus({ section, at, from });
      setEditor((now) => {
        if (now?.key !== key) return now;
        return landed ? null : { ...now, frozen: null };
      });
    });
  };

  const save = () => {
    if (!editor || !resolved) return;
    const value = oneLine(editor.draft);
    if (!value) return discard();
    if ("line" in resolved) {
      if (value === oneLine(resolved.line.text)) return discard();
      send(
        resolved.section,
        { op: "rewrite", index: resolved.line.index, text: value },
        "Saved. Agents load it from their next session.",
        resolved.position,
      );
    } else {
      send(
        resolved.section,
        { op: "add", text: value },
        "Added. Agents load it from their next session.",
        Number.MAX_SAFE_INTEGER,
      );
    }
  };

  const removeInEditor = () => {
    if (!resolved || !("line" in resolved)) return;
    send(
      resolved.section,
      { op: "remove", index: resolved.line.index },
      "Removed.",
      resolved.position,
    );
  };

  const removeRow = (section: string, line: MemoryLine, nth: number, position: number) => {
    const from = doc.blob;
    void edit(
      lineScope(section, nth, line.text),
      section,
      { op: "remove", index: line.index },
      "Removed.",
    ).then((landed) => {
      if (landed) setFocus({ section, at: position, from });
    });
  };

  const decide = (suggestion: MemorySuggestion, decision: "keep" | "discard") =>
    void write(
      "/api/v1/memory/proposals/decide",
      { decisions: [{ proposal: suggestion.proposal_id, decision, base_blob: doc.blob }] },
      decision === "keep" ? "Kept." : "Discarded.",
      proposalScope(suggestion.proposal_id),
    );

  const ctl: Ctl = {
    editor,
    resolved,
    locked,
    refusalFor: (scope) => (refusal?.scope === scope ? refusal.error : null),
    openLine: (section, line, nth) =>
      begin({ section, text: line.text, nth }, line.text, line.memory_id),
    openAdd: (section) => begin({ section, text: null, nth: 0 }, "", null),
    change: (draft) => setEditor((now) => (now ? { ...now, draft } : now)),
    save,
    discard,
    removeInEditor,
    removeRow,
    decide,
  };

  // Back to the draft the dialog interrupted.
  const keepEditing = () => {
    setAsking(null);
    blocker.reset?.();
    requestAnimationFrame(() => root.current?.querySelector("textarea")?.focus());
  };
  const discardChanges = () => {
    if (asking) {
      clearRefusal();
      setEditor(asking);
      setAsking(null);
    } else {
      blocker.proceed?.();
    }
  };

  const all = doc.sections.flatMap((section) => [
    ...section.lines.flatMap((line) => (line.suggestion ? [line.suggestion] : [])),
    ...section.suggestions,
  ]);
  const lines = doc.sections.reduce((sum, section) => sum + section.lines.length, 0);
  const last = [
    ...doc.changes.map((change) => change.at),
    ...doc.sections.flatMap((section) =>
      section.lines.flatMap((line) => (line.provenance ? [line.provenance.at] : [])),
    ),
  ].reduce<string | null>((newest, at) => (newest === null || at > newest ? at : newest), null);

  const toFirst = () => {
    const first = all[0];
    if (!first) return;
    const card = document.getElementById(`sug-${first.proposal_id}`);
    card?.scrollIntoView?.({ behavior: "smooth", block: "center" });
    card?.focus({ preventScroll: true });
  };

  const shared = doc.project_id === FLEET;
  return (
    <div className="m2-wrap">
      <article ref={root} className="m2-doc" aria-busy={pending || undefined}>
        <header className="m2-head">
          <div className="m2-ti">
            {shared ? (
              <LayersIcon />
            ) : (
              <span
                className="pdot"
                style={{ "--pc": colorOf(local, doc.project_id) } as CSSProperties}
              />
            )}
            <h1>{docName(doc)}</h1>
          </div>
          <p className="m2-sub">
            {shared
              ? "What my agents know in every project."
              : `What my agents know about ${doc.name}.`}
            {doc.dirty ? null : " Click any line to change it."}
          </p>
          <div className="m2-stats">
            <span>
              <LinesIcon />
              {plural(lines, "line")}
            </span>
            <span>
              <SparkIcon />
              {all.length > 0 ? (
                <button type="button" className="m2-jump" onClick={toFirst}>
                  {plural(all.length, "suggestion")} to review
                </button>
              ) : (
                "No suggestions to review"
              )}
            </span>
            {last ? (
              <span>
                <ClockIcon />
                Last change {when(last)}
              </span>
            ) : null}
          </div>
          {doc.dirty ? (
            <p className="refusal doc-dirty" role="status">
              <code>{doc.path}</code> has changes that are not committed, so edits here are refused
              until it is committed or reset.
            </p>
          ) : null}
        </header>
        {doc.sections.map((section) => (
          <DocSection key={section.title} section={section} projects={local} ctl={ctl} />
        ))}
      </article>
      {asking || blocker.status === "blocked" ? (
        <DiscardDialog leaving={asking === null} onKeep={keepEditing} onDiscard={discardChanges} />
      ) : null}
    </div>
  );
}

// The page for a document the daemon does not have.
function NotFound({ scope }: { scope: string }) {
  return (
    <div className="m2-wrap">
      <article className="m2-doc">
        <header className="m2-head">
          <div className="m2-ti">
            <h1>Not found</h1>
          </div>
          <p className="m2-sub" role="status">
            There is no memory document called <code>{scope}</code>. Pick one of the tabs above.
          </p>
        </header>
      </article>
    </div>
  );
}

function DocView({ scope }: { scope: string }) {
  const answer = useDocAnswer(scope);
  if (answer === undefined) return null;
  if (answer === null) return <NotFound scope={scope} />;
  return <Doc data={answer} />;
}

// The Memory page (memory.md section 7): one document per project, the shared
// one first, each on its own tab, read and changed in place.
export function MemoryPage({ scope }: { scope: string }) {
  return (
    <div className="docs">
      <Section label="Memory documents">
        <DocTabs current={scope} />
      </Section>
      <Section label="This memory document">
        {/* Keyed, so another document starts with no open editor or refusal. */}
        <DocView key={scope} scope={scope} />
      </Section>
    </div>
  );
}
