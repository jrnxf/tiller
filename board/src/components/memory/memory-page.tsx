import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, ReactNode } from "react";

import type {
  MemoryChange,
  MemoryDocument,
  MemoryLine,
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
import { lisbonDay, lisbonShortDay } from "../health/lisbon";
import { CheckIcon, LayersIcon } from "../shell/icons";
import { Section } from "../states/section";
import { BackIcon, CrossIcon, EditIcon, PlusIcon, RevertIcon, TrashIcon } from "./icons";
import { inlineDiff } from "./inline-diff";
import "./memory.css";

const FLEET = "fleet";

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

// "Wed 4 Nov" and "Wed 4 Nov 17:48", in Lisbon time.
const day = (iso: string) => lisbonShortDay(iso);
const when = (iso: string) => `${lisbonShortDay(iso)} ${lisbonDay(iso).time}`;

const oneLine = (text: string | null | undefined) => (text ?? "").replace(/\s+/g, " ").trim();

type Write = (path: string, body: Record<string, unknown>) => Promise<boolean>;

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
    async (path, body) => {
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
      return landed;
    },
    [queryClient],
  );

  return { write, pending, error };
}

function DocList({ current }: { current: string }) {
  const { data } = useSuspenseQuery(memoryQuery(current === FLEET ? undefined : current));
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const prefs = useStore(prefsStore);
  const local = projects.projects.filter((project) => !isPeerRow(project));
  const shown = new Set(visibleProjects(prefs, local).map((project) => project.id));
  const docs = data.docs.filter(
    (doc) => doc.project_id === FLEET || doc.project_id === current || shown.has(doc.project_id),
  );
  return (
    <nav className="doc-list" aria-label="Memory pages">
      <div className="dl-h">Memory</div>
      {docs.map((doc) => (
        <Link
          key={doc.project_id}
          to="/memory"
          search={doc.project_id === FLEET ? {} : { doc: doc.project_id }}
          className="doc-li"
          aria-current={doc.project_id === current ? "true" : undefined}
          style={{ "--pc": colorOf(local, doc.project_id) } as CSSProperties}
        >
          {doc.project_id === FLEET ? <LayersIcon /> : <span className="pdot" />}
          <span className="nm">{doc.name}</span>
          {doc.suggestions > 0 ? (
            <span className="sn" title={`${plural(doc.suggestions, "suggestion")} waiting`}>
              {doc.suggestions}
            </span>
          ) : (
            <span className="ln" title={plural(doc.lines, "line")}>
              {doc.lines}
            </span>
          )}
        </Link>
      ))}
    </nav>
  );
}

function colorOf(projects: { id: string }[], id: string): string {
  const index = projects.findIndex((project) => project.id === id);
  return index === -1 ? "var(--fg-3)" : projectColor(index);
}

// The quiet hint after a line: its card (or who wrote it) and the day. It
// opens the revision that wrote the line.
function Provenance({ line, onSource }: { line: MemoryLine; onSource: (id: string) => void }) {
  const provenance = line.provenance;
  if (!provenance) return null;
  const who = provenance.card ?? provenance.note;
  return (
    <span className="de-p">
      <button
        className="pcard"
        type="button"
        title="Open the source file, its approval and the sessions that loaded it"
        onClick={() => onSource(provenance.revision_id)}
      >
        {who ? `${who}, ${day(provenance.at)}` : day(provenance.at)}
      </button>
    </span>
  );
}

function budgetText(budget: Record<string, number> | null): string {
  if (!budget) return "";
  const roles = Object.entries(budget).filter(([, tokens]) => typeof tokens === "number");
  if (roles.length === 0) return "";
  const parts = roles.map(([role, tokens]) => `${role} ${tokens.toLocaleString("en-GB")}`);
  return ` Startup prompt after: ${parts.join(", ")} tokens.`;
}

function Why({ suggestion }: { suggestion: MemorySuggestion }) {
  const from = suggestion.card ? ` from ${suggestion.card}` : "";
  const what = suggestion.op === "retire" ? "Removal suggested" : "Suggested";
  return (
    <div className="de-why">
      {what} by {suggestion.by}
      {from}, {when(suggestion.at)}.{suggestion.why ? ` ${suggestion.why}` : ""}
      <span className="de-budget">{budgetText(suggestion.token_budget_after)}</span>
    </div>
  );
}

function SuggestionRow({
  suggestion,
  line,
  active,
  locked,
  onDecide,
}: {
  suggestion: MemorySuggestion;
  line: MemoryLine | null;
  active: boolean;
  locked: boolean;
  onDecide: (suggestion: MemorySuggestion, decision: "keep" | "discard") => void;
}) {
  let body: ReactNode;
  let kind: string;
  // The line it names: the row it sits on, or the text the view sends when it
  // is listed apart from that row.
  const current = line?.text ?? suggestion.target;
  const { gate } = suggestion;
  const gateId = `gate-${suggestion.proposal_id}`;
  if (suggestion.op === "add") {
    kind = "add";
    body = <span className="de-t sg">{suggestion.text}</span>;
  } else if (current === null) {
    kind = suggestion.op === "rewrite" ? "edit" : "retire";
    body = (
      <span className="de-t">
        {suggestion.op === "rewrite" ? (
          <ins>{suggestion.text}</ins>
        ) : (
          "The line it removes is no longer in the file."
        )}
      </span>
    );
  } else if (suggestion.op === "rewrite") {
    kind = "edit";
    body = (
      <span className="de-t">
        {inlineDiff(current, suggestion.text ?? current).map((part, i) => {
          const gap = i > 0 ? " " : "";
          if (part.kind === "del") return [gap, <del key={i}>{part.text}</del>];
          if (part.kind === "ins") return [gap, <ins key={i}>{part.text}</ins>];
          return `${gap}${part.text}`;
        })}
      </span>
    );
  } else {
    kind = "retire";
    body = (
      <span className="de-t">
        <del>{current}</del>
      </span>
    );
  }
  return (
    <div
      className={`de sug ${kind}`}
      tabIndex={0}
      data-active={active}
      data-proposal={suggestion.proposal_id}
    >
      <span className="de-bu">{kind === "add" ? <PlusIcon /> : null}</span>
      <div className="de-body">
        {body}
        <Why suggestion={suggestion} />
        {gate.ok ? null : (
          <div className="de-gate" id={gateId}>
            Keep waits for the holdout gate: {gate.reason}.
            {gate.hint ? (
              <>
                {" "}
                Next: <span className="de-hint">{gate.hint}</span>
              </>
            ) : null}
          </div>
        )}
      </div>
      <div className="sugbar" role="group" aria-label={`Suggestion from ${suggestion.by}`}>
        <button
          className="sb keep"
          type="button"
          disabled={locked || !gate.ok}
          aria-describedby={gate.ok ? undefined : gateId}
          onClick={() => onDecide(suggestion, "keep")}
        >
          <CheckIcon />
          Keep
        </button>
        <button
          className="sb"
          type="button"
          disabled={locked}
          onClick={() => onDecide(suggestion, "discard")}
        >
          Discard
        </button>
      </div>
    </div>
  );
}

// One line of prose, edited in place: Enter keeps the edit, Esc puts the text
// back. A blank line is not saved; the bin removes it.
function LineRow({
  line,
  locked,
  onSource,
  onRewrite,
  onRemove,
}: {
  line: MemoryLine;
  locked: boolean;
  onSource: (id: string) => void;
  onRewrite: (line: MemoryLine, text: string) => Promise<boolean>;
  onRemove: (line: MemoryLine) => void;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  const commit = () => {
    const el = ref.current;
    if (!el) return;
    const text = oneLine(el.textContent);
    if (text === oneLine(line.text)) return;
    if (!text) {
      el.textContent = line.text;
      return;
    }
    void onRewrite(line, text).then((landed) => {
      if (!landed && ref.current) ref.current.textContent = line.text;
    });
  };
  const onKeyDown = (event: KeyboardEvent<HTMLSpanElement>) => {
    if (event.key === "Enter") {
      event.preventDefault();
      event.currentTarget.blur();
    } else if (event.key === "Escape") {
      event.currentTarget.textContent = line.text;
      event.currentTarget.blur();
    }
  };
  return (
    <div className="de">
      <span className="de-bu" />
      <div className="de-body">
        <span
          ref={ref}
          className="de-t"
          contentEditable={locked ? undefined : "plaintext-only"}
          suppressContentEditableWarning
          spellCheck={false}
          role="textbox"
          aria-label="Edit this line"
          aria-readonly={locked || undefined}
          data-ph="Write a line..."
          onBlur={commit}
          onKeyDown={onKeyDown}
        >
          {line.text}
        </span>
        <Provenance line={line} onSource={onSource} />
      </div>
      <button
        className="de-x"
        type="button"
        title="Remove this line"
        aria-label="Remove this line"
        disabled={locked}
        onClick={() => onRemove(line)}
      >
        <TrashIcon />
      </button>
    </div>
  );
}

function NewLine({ onDone }: { onDone: (text: string) => void }) {
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => ref.current?.focus(), []);
  return (
    <div className="de">
      <span className="de-bu" />
      <div className="de-body">
        <span
          ref={ref}
          className="de-t"
          contentEditable="plaintext-only"
          suppressContentEditableWarning
          spellCheck={false}
          role="textbox"
          aria-label="New line"
          data-ph="Write a line..."
          onBlur={(event) => onDone(oneLine(event.currentTarget.textContent))}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              event.currentTarget.blur();
            } else if (event.key === "Escape") {
              event.currentTarget.textContent = "";
              event.currentTarget.blur();
            }
          }}
        />
      </div>
    </div>
  );
}

function DocSection({
  doc,
  section,
  firstSuggestion,
  locked,
  write,
  onSource,
  onDecide,
}: {
  doc: MemoryDocument;
  section: MemorySection;
  firstSuggestion: string | null;
  locked: boolean;
  write: Write;
  onSource: (id: string) => void;
  onDecide: (suggestion: MemorySuggestion, decision: "keep" | "discard") => void;
}) {
  const [adding, setAdding] = useState(false);
  const edit = (body: Record<string, unknown>) =>
    write("/api/v1/memory/edits", {
      project: doc.project_id,
      section: section.title,
      base_blob: doc.blob,
      ...body,
    });
  const empty = section.lines.length === 0 && section.suggestions.length === 0;
  return (
    <section className="doc-sec" aria-label={section.title}>
      <h2>{section.title}</h2>
      {section.lines.map((line) =>
        line.suggestion ? (
          <SuggestionRow
            key={`${line.index}:${line.suggestion.proposal_id}`}
            suggestion={line.suggestion}
            line={line}
            active={line.suggestion.proposal_id === firstSuggestion}
            locked={locked}
            onDecide={onDecide}
          />
        ) : (
          // Keyed by its text, so a landed edit or a refetch starts the line
          // afresh instead of keeping what the browser typed into it.
          <LineRow
            key={`${line.index}:${line.text}`}
            line={line}
            locked={locked}
            onSource={onSource}
            onRewrite={(target, text) => edit({ op: "rewrite", index: target.index, text })}
            onRemove={(target) => void edit({ op: "remove", index: target.index })}
          />
        ),
      )}
      {section.suggestions.map((suggestion) => (
        <SuggestionRow
          key={suggestion.proposal_id}
          suggestion={suggestion}
          line={null}
          active={suggestion.proposal_id === firstSuggestion}
          locked={locked}
          onDecide={onDecide}
        />
      ))}
      {empty && !adding ? (
        <p className="de-empty">
          Nothing here yet. Agents suggest lines as they learn, or add your own.
        </p>
      ) : null}
      {adding ? (
        <NewLine
          onDone={(text) => {
            if (!text) {
              setAdding(false);
              return;
            }
            // A refused line stays open with its text, to fix or retry.
            void edit({ op: "add", text }).then((landed) => landed && setAdding(false));
          }}
        />
      ) : null}
      <button className="add-line" type="button" disabled={locked} onClick={() => setAdding(true)}>
        <PlusIcon />
        Add a line
      </button>
    </section>
  );
}

const CHANGE_LABEL: Record<MemoryChange["kind"], string> = {
  kept: "Added a line",
  added: "Added a line",
  edit: "Edited a line",
  discard: "Discarded a suggestion",
  remove: "Removed a line",
};

const CHANGE_ICON: Record<MemoryChange["kind"], () => ReactNode> = {
  kept: PlusIcon,
  added: PlusIcon,
  edit: EditIcon,
  discard: CrossIcon,
  remove: TrashIcon,
};

function RecentChanges({
  changes,
  locked,
  onUndo,
}: {
  changes: MemoryChange[];
  locked: boolean;
  onUndo: (change: MemoryChange) => void;
}) {
  return (
    <>
      <div className="rail-h">Recent changes</div>
      {changes.length === 0 ? (
        <p className="chg-none">
          Nothing has changed yet. Edits and kept suggestions show up here, each with an Undo.
        </p>
      ) : (
        changes.map((change) => {
          const Icon = CHANGE_ICON[change.kind];
          return (
            <div className="chg" key={change.proposal_id} data-proposal={change.proposal_id}>
              <span className={`chg-i ${change.kind}`}>
                <Icon />
              </span>
              <div className="chg-b">
                <div className="chg-t">{CHANGE_LABEL[change.kind]}</div>
                {change.text ? <div className="chg-q">{change.text}</div> : null}
                <div className="chg-m">
                  {change.from === "you" ? "By you" : `From ${change.from}`}, {when(change.at)}
                </div>
              </div>
              <button
                className="undo"
                type="button"
                title="Undo this change"
                disabled={locked}
                onClick={() => onUndo(change)}
              >
                <RevertIcon />
                Undo
              </button>
            </div>
          );
        })
      )}
    </>
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
      <div className="rail-h">Loaded by</div>
      {revision.sessions.length === 0 ? (
        <p className="chg-none">No session has loaded this revision yet.</p>
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
      <div className="rail-h">File at this revision</div>
      {revision.text === null ? (
        <p className="chg-none">This revision is no longer in git.</p>
      ) : (
        <pre className="rev-text">{revision.text.trimEnd()}</pre>
      )}
    </div>
  );
}

function Rail({
  doc,
  revision,
  locked,
  onBack,
  onUndo,
}: {
  doc: MemoryDocument;
  revision: string | null;
  locked: boolean;
  onBack: () => void;
  onUndo: (change: MemoryChange) => void;
}) {
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    // On a phone the rail sits below the document: bring the source into view.
    if (revision) ref.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  }, [revision]);
  return (
    <aside
      ref={ref}
      className="doc-rail"
      aria-label={revision ? "Source of this line" : "Recent changes"}
    >
      {revision ? (
        <>
          <button className="rev-back" type="button" onClick={onBack}>
            <BackIcon />
            Recent changes
          </button>
          <Section label="This revision">
            <RevisionDetail id={revision} />
          </Section>
        </>
      ) : (
        <RecentChanges changes={doc.changes} locked={locked} onUndo={onUndo} />
      )}
    </aside>
  );
}

function DocView({ scope }: { scope: string }) {
  const { data } = useSuspenseQuery(memoryQuery(scope === FLEET ? undefined : scope));
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const doc = data.doc;
  const { write, pending, error } = useMemoryWrite();
  const [revision, setRevision] = useState<string | null>(null);
  const locked = doc.dirty || pending;
  const local = projects.projects.filter((project) => !isPeerRow(project));

  const all = doc.sections.flatMap((section) => [
    ...section.lines.flatMap((line) => (line.suggestion ? [line.suggestion] : [])),
    ...section.suggestions,
  ]);
  const onDecide = (suggestion: MemorySuggestion, decision: "keep" | "discard") =>
    void write("/api/v1/memory/proposals/decide", {
      decisions: [{ proposal: suggestion.proposal_id, decision, base_blob: doc.blob }],
    });
  const onUndo = (change: MemoryChange) =>
    void write(`/api/v1/memory/changes/${change.proposal_id}/undo`, { base_blob: doc.blob });

  const shared = doc.project_id === FLEET;
  return (
    <>
      <div className="doc-main">
        <article className="doc" aria-busy={pending || undefined}>
          <header className="doc-h">
            <div className="doc-ti">
              {shared ? (
                <LayersIcon />
              ) : (
                <span
                  className="pdot"
                  style={{ "--pc": colorOf(local, doc.project_id) } as CSSProperties}
                />
              )}
              <h1>{doc.name}</h1>
            </div>
            <p className="doc-sub">
              {shared
                ? "What my agents know in every project."
                : `What my agents know about ${doc.name}.`}{" "}
              {doc.dirty ? null : <b>Click any line to change it.</b>}
            </p>
            {doc.dirty ? (
              <p className="refusal doc-dirty" role="status">
                <code>{doc.path}</code> has changes that are not committed, so edits here are
                refused until it is committed or reset.
              </p>
            ) : null}
            <div className="doc-legend">
              {all.length > 0 ? (
                <>
                  <span className="lg-s">
                    <ins>Tinted text</ins>
                  </span>
                  <span>
                    {plural(all.length, "suggestion")} from agents. Keep or discard each one right
                    where it appears; nothing changes until you do.
                  </span>
                </>
              ) : (
                <span>
                  No suggestions right now. When an agent learns something, it appears here in
                  tinted text for you to keep or discard.
                </span>
              )}
            </div>
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
              firstSuggestion={all[0]?.proposal_id ?? null}
              locked={locked}
              write={write}
              onSource={setRevision}
              onDecide={onDecide}
            />
          ))}
        </article>
      </div>
      <Rail
        doc={doc}
        revision={revision}
        locked={locked}
        onBack={() => setRevision(null)}
        onUndo={onUndo}
      />
    </>
  );
}

// The Memory page (memory.md section 7): one document per project, the shared
// one first, read and changed in place.
export function MemoryPage({ scope }: { scope: string }) {
  return (
    <div className="docs">
      <Section label="Memory pages">
        <DocList current={scope} />
      </Section>
      <Section label="This memory document">
        {/* Keyed, so another document starts with no open source or refusal. */}
        <DocView key={scope} scope={scope} />
      </Section>
    </div>
  );
}
