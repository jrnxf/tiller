import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useCallback, useEffect, useRef, useState } from "react";
import type {
  CSSProperties,
  Dispatch,
  KeyboardEvent,
  MouseEvent,
  ReactNode,
  RefObject,
  SetStateAction,
} from "react";

import type {
  MemoryDocument,
  MemoryLine,
  MemoryProvenance,
  MemoryRevision,
  MemorySection,
  MemorySuggestion,
} from "../../lib/api-types";
import { isPeerRow } from "../../lib/peers";
import { prefsStore, visibleProjects } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import { API_KEY_PREFIX, memoryQuery, projectsQuery, revisionQuery } from "../../lib/queries";
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
import { useNamedProjects } from "../board/peer-state";

const FLEET = "fleet";

// The shared document lives on the `fleet` project, which the board calls
// "Host" elsewhere; here it holds what every project shares.
const docName = (doc: { project_id: string; name: string }) =>
  doc.project_id === FLEET ? "All projects" : doc.name;

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

// "Wed 4 Nov" and "Wed 4 Nov, 17:48", in Lisbon time.
const day = (iso: string) => lisbonShortDay(iso);
const when = (iso: string) => `${lisbonShortDay(iso)}, ${lisbonDay(iso).time}`;

const oneLine = (text: string | null | undefined) => (text ?? "").replace(/\s+/g, " ").trim();

// The note a landed change shows, so the page says it worked.
type Write = (path: string, body: Record<string, unknown>, landed: string) => Promise<boolean>;

// Every change names the blob the page showed, so a click that lands twice, or
// one made against a text that moved on, is refused instead of applied again.
// Each act therefore gets its own idempotency key. Landed or refused, the
// document is read again: the change shows, or the text it was refused against.
function useMemoryWrite() {
  const queryClient = useQueryClient();
  const inFlight = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ActionError | null>(null);

  const write = useCallback<Write>(
    async (path, body, note) => {
      if (inFlight.current) return false;
      inFlight.current = true;
      setPending(true);
      setError(null);
      let landed = false;
      try {
        await sendMutation("POST", path, { idempotency_key: ulid(), ...body });
        landed = true;
      } catch (caught) {
        setError(
          caught instanceof ActionError
            ? caught
            : new ActionError(0, "internal", "Something went wrong, so this was not saved."),
        );
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

  return { write, pending, error };
}

function colorOf(projects: { id: string }[], id: string): string {
  const index = projects.findIndex((project) => project.id === id);
  return index === -1 ? "var(--fg-3)" : projectColor(index);
}

// One tab per memory document, the shared one first. A tab shows the
// suggestions waiting on it, or its line count when there are none.
function DocTabs({ current }: { current: string }) {
  const { data } = useSuspenseQuery(memoryQuery(current === FLEET ? undefined : current));
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const prefs = useStore(prefsStore);
  const local = projects.projects.filter((project) => !isPeerRow(project));
  const shown = new Set(visibleProjects(prefs, local).map((project) => project.id));
  const docs = data.docs.filter(
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

// Puts focus back on `ref` when the editor it opened closes and took focus
// with it, so a keyboard user carries on from the same place.
function useFocusBack(open: boolean, ref: RefObject<HTMLElement | null>) {
  const was = useRef(open);
  useEffect(() => {
    const lost = document.activeElement === null || document.activeElement === document.body;
    if (was.current && !open && lost) ref.current?.focus({ preventScroll: true });
    was.current = open;
  }, [open, ref]);
}

// The editor a line opens in place, and Add a line opens at the end of its
// section. Enter saves and Esc cancels; a blank line is never saved.
function LineEditor({
  initial,
  label,
  meta,
  save,
  locked,
  onSave,
  onCancel,
  onRemove,
}: {
  initial: string;
  label: string;
  meta: ReactNode;
  save: string;
  locked: boolean;
  onSave: (text: string) => void;
  onCancel: () => void;
  onRemove?: () => void;
}) {
  const [text, setText] = useState(initial);
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // An input method's Enter picks a candidate; it does not save.
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    } else if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      if (!locked) onSave(text);
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
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={onKeyDown}
      />
      <div className="ed-f">
        <span className="ed-meta">{meta}</span>
        <span className="ed-keys" aria-hidden="true">
          <kbd>↵</kbd>save
          <span className="gap" />
          <kbd>Esc</kbd>cancel
        </span>
        <span className="ed-b">
          {onRemove ? (
            <button type="button" className="btn ghost ed-rm" disabled={locked} onClick={onRemove}>
              <TrashIcon />
              Remove
            </button>
          ) : null}
          <button type="button" className="btn ghost grow" onClick={onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className="btn primary"
            disabled={locked}
            onClick={() => onSave(text)}
          >
            {save}
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
  onEdit,
  onRemove,
}: {
  line: MemoryLine;
  projects: { id: string }[];
  editing: boolean;
  locked: boolean;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const edit = useRef<HTMLButtonElement>(null);
  useFocusBack(editing, edit);
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
          ref={edit}
          type="button"
          className="ib"
          title="Edit this line"
          aria-label="Edit this line"
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
  onDecide,
}: {
  suggestion: MemorySuggestion;
  line: MemoryLine | null;
  section: string;
  locked: boolean;
  onDecide: (suggestion: MemorySuggestion, decision: "keep" | "discard") => void;
}) {
  // The line it names: the row it sits under, or the text the view sends when
  // it is listed apart from that row.
  const current = line?.text ?? suggestion.target;
  const where = line ? "of the line above" : "of a line in this section";
  const { gate } = suggestion;
  const gateId = `gate-${suggestion.proposal_id}`;
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
        {from}, {when(suggestion.at)}.{suggestion.why ? ` ${suggestion.why}` : ""}
        {budget ? <span className="bud"> {budget}</span> : null}
      </div>
      {gate.ok ? null : (
        <div className="gate" id={gateId}>
          <WarnIcon />
          <span>
            Keep waits for the holdout gate: {gate.reason}.
            {gate.hint ? (
              <span className="gate-run">
                {" "}
                Next: <code>{gate.hint}</code>
              </span>
            ) : null}
          </span>
        </div>
      )}
      <div className="sug-f">
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
        <button
          type="button"
          className="btn ghost"
          disabled={locked}
          onClick={() => onDecide(suggestion, "discard")}
        >
          Discard
        </button>
        {gate.ok ? <span className="note">Nothing changes until you keep it.</span> : null}
      </div>
    </div>
  );
}

// Which editor is open: a line's, by its text and which copy of that text it
// is in the section (an index shifts when a line above changes), or the
// section's new line (`text` null).
type Open = { section: string; text: string | null; nth: number };

const sameOpen = (a: Open | null, b: Open) =>
  a !== null && a.section === b.section && a.text === b.text && a.nth === b.nth;

// Each line with which copy of its text it is, counted from the top.
const numbered = (lines: MemoryLine[]) => {
  const seen = new Map<string, number>();
  return lines.map((line) => {
    const nth = seen.get(line.text) ?? 0;
    seen.set(line.text, nth + 1);
    return { line, nth };
  });
};

// Whether the line `open` names is still in `doc`.
const stillThere = (doc: MemoryDocument, open: Open) =>
  doc.sections.some(
    (section) =>
      section.title === open.section &&
      numbered(section.lines).some(({ line, nth }) =>
        sameOpen(open, { section: section.title, text: line.text, nth }),
      ),
  );

function DocSection({
  doc,
  section,
  projects,
  open,
  locked,
  write,
  onOpen,
  onDecide,
}: {
  doc: MemoryDocument;
  section: MemorySection;
  projects: { id: string }[];
  open: Open | null;
  locked: boolean;
  write: Write;
  onOpen: Dispatch<SetStateAction<Open | null>>;
  onDecide: (suggestion: MemorySuggestion, decision: "keep" | "discard") => void;
}) {
  const addButton = useRef<HTMLButtonElement>(null);
  const fresh: Open = { section: section.title, text: null, nth: 0 };
  const adding = sameOpen(open, fresh);
  useFocusBack(adding, addButton);
  const edit = (body: Record<string, unknown>, note: string) =>
    write(
      "/api/v1/memory/edits",
      { project: doc.project_id, section: section.title, base_blob: doc.blob, ...body },
      note,
    );
  const close = () => onOpen(null);
  // A landed change closes its own editor and leaves any other open. A
  // refused one leaves its editor open with the text, to fix or retry.
  const settle = (target: Open, landed: Promise<boolean>) =>
    void landed.then((ok) => ok && onOpen((now) => (sameOpen(now, target) ? null : now)));
  const remove = (line: MemoryLine, at: Open) =>
    settle(at, edit({ op: "remove", index: line.index }, "Removed."));
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
      {numbered(section.lines).map(({ line, nth }) => {
        const at: Open = { section: section.title, text: line.text, nth };
        const editing = sameOpen(open, at);
        return (
          // Keyed by its text, so a landed edit starts the line afresh, and a
          // change above it keeps an open editor's draft.
          <div className="a-item" key={`${nth}:${line.text}`}>
            <LineRow
              line={line}
              projects={projects}
              editing={editing}
              locked={locked}
              onEdit={() => onOpen(at)}
              onRemove={() => remove(line, at)}
            />
            {editing ? (
              <div className="a-edrow">
                <LineEditor
                  initial={line.text}
                  label="Edit this line"
                  meta={<SourceMeta line={line} projects={projects} />}
                  save="Save"
                  locked={locked}
                  onCancel={close}
                  onRemove={() => remove(line, at)}
                  onSave={(text) => {
                    const value = oneLine(text);
                    if (!value || value === oneLine(line.text)) return close();
                    settle(
                      at,
                      edit(
                        { op: "rewrite", index: line.index, text: value },
                        "Saved. Agents load it from their next session.",
                      ),
                    );
                  }}
                />
              </div>
            ) : null}
            {line.suggestion ? (
              <SuggestionCard
                suggestion={line.suggestion}
                line={line}
                section={section.title}
                locked={locked}
                onDecide={onDecide}
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
          locked={locked}
          onDecide={onDecide}
        />
      ))}
      {empty && !adding ? (
        <p className="a-empty">
          Nothing here yet. Agents suggest lines as they learn, or add your own.
        </p>
      ) : null}
      {adding ? (
        <div className="a-edrow">
          <LineEditor
            initial=""
            label="New line"
            meta={
              <>
                <span>Adds to {section.title} as</span>
                <span className="who">
                  <span className="m2av you">Y</span>
                  <span className="nm">you</span>
                </span>
              </>
            }
            save="Add line"
            locked={locked}
            onCancel={close}
            onSave={(text) => {
              const value = oneLine(text);
              if (!value) return close();
              settle(
                fresh,
                edit({ op: "add", text: value }, "Added. Agents load it from their next session."),
              );
            }}
          />
        </div>
      ) : (
        <button
          ref={addButton}
          className="a-add"
          type="button"
          disabled={locked}
          onClick={() => onOpen(fresh)}
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

function DocView({ scope }: { scope: string }) {
  const { data } = useSuspenseQuery(memoryQuery(scope === FLEET ? undefined : scope));
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const doc = data.doc;
  const { write, pending, error } = useMemoryWrite();
  const [open, setOpen] = useState<Open | null>(null);
  // A line that changed under its open editor (a kept suggestion)
  // takes the editor with it.
  if (open !== null && open.text !== null && !stillThere(doc, open)) setOpen(null);
  const locked = doc.dirty || pending;
  const local = projects.projects.filter((project) => !isPeerRow(project));

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

  const onDecide = (suggestion: MemorySuggestion, decision: "keep" | "discard") =>
    void write(
      "/api/v1/memory/proposals/decide",
      { decisions: [{ proposal: suggestion.proposal_id, decision, base_blob: doc.blob }] },
      decision === "keep" ? "Kept." : "Discarded.",
    );
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
      <article className="m2-doc" aria-busy={pending || undefined}>
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
          {error ? (
            <p className="refusal" role="alert">
              {error.message}
            </p>
          ) : null}
        </header>
        {doc.sections.map((section) => (
          <DocSection
            key={section.title}
            doc={doc}
            section={section}
            projects={local}
            open={open}
            locked={locked}
            write={write}
            onOpen={setOpen}
            onDecide={onDecide}
          />
        ))}
      </article>
    </div>
  );
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
