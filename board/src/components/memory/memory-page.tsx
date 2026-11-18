import { useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import type { CSSProperties, FormEvent } from "react";

import type { LiveNote, MemoryKind, MemoryResponse, PendingNote } from "../../lib/api-types";
import { isPeerRow } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { memoryQuery, projectsQuery } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { WarnIcon } from "../board/icons";
import { useNamedProjects } from "../board/peer-state";
import { showToast } from "../board/toast";
import { Section } from "../states/section";
import { Select } from "../ui/select";
import { Inline, plural, useMemoryRefresh, when } from "./parts";
import { Retros } from "./retros";
import "./memory.css";

// The Memory page (memory.md section 4). Pending is tiller's queue of notes
// the fleet suggested, each waiting for Keep, Edit or Discard. Live is what a
// launch of one role in one project loads from kb now, and what it skips.

export type MemoryTab = "pending" | "live";

const ROLES = ["builder", "reviewer", "verifier", "scout", "orchestrator", "curator"];
const KINDS: MemoryKind[] = ["convention", "decision", "gotcha", "how-to"];
const TITLE_MAX = 120;
const BODY_MAX = 2000;
const ALL = "*";

const capital = (text: string) => `${text.charAt(0).toUpperCase()}${text.slice(1)}`;
const short = (id: string) => id.slice(-4).toUpperCase();
const tokens = (n: number) => n.toLocaleString("en-US");

interface Named {
  id: string;
  name: string;
  color: string;
}

// This host's projects, by the names and colours the board gives them.
function useProjects(): Named[] {
  const { projects } = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  return projects
    .filter((project) => !isPeerRow(project))
    .map((project, index) => ({ id: project.id, name: project.name, color: projectColor(index) }));
}

function useSearchTo() {
  const navigate = useNavigate();
  return (search: { project?: string; role?: string; tab?: MemoryTab }) =>
    navigate({ to: "/memory", search: (prev) => ({ ...prev, ...search }) });
}

// Who suggested a pending note.
function by(note: PendingNote): string {
  if (note.source === "curator") return "by the curator";
  if (note.source === "migration") return "from the old memory";
  return note.role ? `by ${note.role}` : "by the fleet";
}

// Where a pending note came from: its card, run and retro. The board has no
// run page, so a run opens its card.
function Origin({ note }: { note: PendingNote }) {
  const { card_id: card, run_id: run, retro_id: retro } = note;
  const parts = [];
  if (card) {
    parts.push(
      <Link key="card" to="/cards/$cardId" params={{ cardId: card }}>
        card {short(card)}
      </Link>,
    );
  }
  if (run) {
    parts.push(
      card ? (
        <Link key="run" to="/cards/$cardId" params={{ cardId: card }}>
          run {short(run)}
        </Link>
      ) : (
        <span key="run">run {short(run)}</span>
      ),
    );
  }
  if (retro) parts.push(<span key="retro">retro {short(retro)}</span>);
  if (parts.length === 0) return null;
  return (
    <p className="mem-origin">
      From{" "}
      {parts.flatMap((part, index) =>
        index === 0 ? [part] : [<span key={index}> · </span>, part],
      )}
    </p>
  );
}

type Write = "edit" | "keep" | "discard";

function NoteCard({ note, project }: { note: PendingNote; project: Named | undefined }) {
  const refresh = useMemoryRefresh();
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState<Write | null>(null);
  const [error, setError] = useState<ActionError | null>(null);
  // The draft keeps the rev it was opened at, so an edit that lands
  // elsewhere meanwhile turns its Save into a conflict, not an overwrite.
  const [draft, setDraft] = useState({
    title: note.title,
    body: note.body,
    kind: note.kind,
    rev: note.rev,
  });

  async function send(write: Write, method: "POST" | "PATCH", path: string, fields = {}) {
    setBusy(write);
    setError(null);
    try {
      await sendMutation(method, path, { idempotency_key: ulid(), rev: note.rev, ...fields });
      return true;
    } catch (caught) {
      setError(
        caught instanceof ActionError ? caught : new ActionError(0, "failed", String(caught)),
      );
      return false;
    } finally {
      setBusy(null);
      void refresh();
    }
  }

  const base = `/api/v1/memory/pending/${note.id}`;
  const keep = async () => {
    if (await send("keep", "POST", `${base}/keep`)) showToast("ok", "Kept in kb.");
  };
  const discard = async () => {
    if (await send("discard", "POST", `${base}/discard`)) showToast("ok", "Discarded.");
  };
  const startEdit = () => {
    setDraft({ title: note.title, body: note.body, kind: note.kind, rev: note.rev });
    setError(null);
    setEditing(true);
  };
  const save = async (event: FormEvent) => {
    event.preventDefault();
    const changed = {
      ...(draft.title !== note.title ? { title: draft.title } : {}),
      ...(draft.body !== note.body ? { body: draft.body } : {}),
      ...(draft.kind !== note.kind ? { kind: draft.kind } : {}),
    };
    if (Object.keys(changed).length === 0) {
      setEditing(false);
      return;
    }
    if (await send("edit", "PATCH", base, { ...changed, rev: draft.rev })) setEditing(false);
  };

  const label = `"${note.title}"`;
  const blank = draft.title.trim() === "" || draft.body.trim() === "";
  return (
    <li className="mem-card" data-note={note.id}>
      {editing ? (
        <form className="mem-edit" aria-label={`Edit ${label}`} onSubmit={save}>
          <div className="mem-edit-row">
            <label className="mem-field mem-field-grow">
              <span>Title</span>
              <input
                value={draft.title}
                maxLength={TITLE_MAX}
                onChange={(event) => setDraft({ ...draft, title: event.target.value })}
              />
            </label>
            <div className="mem-field">
              <span>Kind</span>
              <Select
                value={draft.kind}
                label="Kind"
                className="mem-kind"
                onValueChange={(kind) => setDraft({ ...draft, kind: kind as MemoryKind })}
                options={KINDS.map((kind) => ({ value: kind, label: capital(kind) }))}
              />
            </div>
          </div>
          <label className="mem-field">
            <span>Body</span>
            <textarea
              value={draft.body}
              rows={4}
              maxLength={BODY_MAX}
              onChange={(event) => setDraft({ ...draft, body: event.target.value })}
            />
          </label>
          <div className="mem-acts">
            <button
              type="button"
              className="btn sm"
              disabled={busy !== null}
              onClick={() => {
                setEditing(false);
                setError(null);
              }}
            >
              Cancel
            </button>
            <button type="submit" className="btn primary sm" disabled={busy !== null || blank}>
              Save
            </button>
          </div>
        </form>
      ) : (
        <>
          <div className="mem-main">
            <h3 className="mem-title">{note.title}</h3>
            <p className="mem-meta">
              <span className="lab sm">{note.kind}</span>
              {project ? (
                <span className="mem-proj" style={{ "--pc": project.color } as CSSProperties}>
                  {project.name}
                </span>
              ) : null}
              <span>
                {by(note)} · <time dateTime={note.created_at}>{when(note.created_at)}</time>
                {note.edited_at ? " · edited" : ""}
              </span>
            </p>
            <p className="mem-body">{note.body}</p>
            <Origin note={note} />
          </div>
          <div className="mem-acts">
            <button
              type="button"
              className="btn sm"
              disabled={busy !== null}
              aria-label={`Discard ${label}`}
              onClick={discard}
            >
              Discard
            </button>
            <button
              type="button"
              className="btn sm"
              disabled={busy !== null}
              aria-label={`Edit ${label}`}
              onClick={startEdit}
            >
              Edit
            </button>
            <button
              type="button"
              className="btn primary sm"
              disabled={busy !== null}
              aria-label={`Keep ${label}`}
              onClick={keep}
            >
              Keep
            </button>
          </div>
        </>
      )}
      {error ? (
        <div className="mem-refusal">
          <Inline error={error} />
        </div>
      ) : null}
    </li>
  );
}

function Empty({ title, children }: { title: string; children: string }) {
  return (
    <div className="mem-empty">
      <p className="mem-empty-t">{title}</p>
      <p>{children}</p>
    </div>
  );
}

function Pending({ read, projects }: { read: MemoryResponse; projects: Named[] }) {
  const named = projects.find((project) => project.id === read.project_id);
  return (
    <div role="tabpanel" id="mem-panel-pending" aria-labelledby="mem-tab-pending">
      {read.pending.length === 0 ? (
        <Empty title="Nothing waits for you">
          When a session or the curator suggests a note, it waits here until you keep or discard it.
        </Empty>
      ) : (
        <ul className="mem-list" aria-label="Pending notes">
          {read.pending.map((note) => (
            // Keyed by id, so an edit that lands elsewhere keeps an open draft:
            // its Save then meets the new rev and says so.
            <NoteCard
              key={note.id}
              note={note}
              project={
                read.project_id === null
                  ? projects.find((each) => each.id === note.project_id)
                  : undefined
              }
            />
          ))}
        </ul>
      )}
      <Section label="Retros">
        <Retros project={read.project_id ?? undefined} name={named?.name ?? "All projects"} />
      </Section>
    </div>
  );
}

// The fleet loads nothing from kb on a host where kb is missing or broken.
function KbWarning({ read }: { read: MemoryResponse }) {
  const missing = read.kb.state === "missing";
  return (
    <div className="mem-warn" role="alert">
      <WarnIcon />
      <div>
        <p className="mem-warn-t">
          {missing ? "tiller cannot find kb on this host" : "tiller cannot read kb on this host"}
        </p>
        <p>
          The fleet loads no notes on this host until kb works again, and Keep cannot save a note.
          kb home: <code>{read.kb.home}</code>
        </p>
      </div>
    </div>
  );
}

function LiveRow({ note }: { note: LiveNote }) {
  return (
    <li className="mem-row" data-loaded={note.loaded}>
      <div className="mem-row-main">
        <span className="mem-row-t">{note.title}</span>
        <span className="mem-row-m">
          {note.scope} · {tokens(note.tokens)} {note.tokens === 1 ? "token" : "tokens"}
          {note.loaded ? null : (
            <span className="mem-skip"> · Skipped: {note.reason ?? "not loaded"}</span>
          )}
        </span>
      </div>
      <span className="lab sm">{note.kind}</span>
    </li>
  );
}

function Live({ read }: { read: MemoryResponse }) {
  const to = useSearchTo();
  const { used, limit } = read.budget;
  const width = `${Math.min(100, Math.round((used / Math.max(1, limit)) * 100))}%`;
  const skipped = read.live.filter((note) => !note.loaded).length;
  const ok = read.kb.state === "ok";
  return (
    <div role="tabpanel" id="mem-panel-live" aria-labelledby="mem-tab-live">
      {ok ? null : <KbWarning read={read} />}
      {read.project_id === null ? (
        <Empty title="Pick a project">
          Live shows what a session of one project loads from kb when it starts.
        </Empty>
      ) : (
        <section className="mem-live" aria-label="Notes a launch loads">
          <div className="mem-live-head">
            <div className="mem-budget">
              <span className="mem-budget-l">{capital(read.role)} startup</span>
              <span
                className="mem-bar"
                role="meter"
                aria-label="Startup budget"
                aria-valuemin={0}
                aria-valuemax={limit}
                aria-valuenow={used}
              >
                <i style={{ width }} />
              </span>
              <span className="mem-budget-n">
                {tokens(used)} / {tokens(limit)} tokens
              </span>
            </div>
            <Select
              value={read.role}
              label="Role"
              className="mem-role"
              onValueChange={(role) => void to({ role })}
              options={ROLES.map((role) => ({ value: role, label: capital(role) }))}
            />
          </div>
          {read.live.length === 0 ? (
            <p className="mem-none">
              {ok ? "kb has no notes for this project yet." : "No notes load while kb is down."}
            </p>
          ) : (
            <>
              <ul className="mem-rows" aria-label="Notes">
                {read.live.map((note) => (
                  <LiveRow key={note.id} note={note} />
                ))}
              </ul>
              <p className="mem-foot">
                {skipped > 0 ? `${plural(skipped, "note")} skipped. ` : ""}Open work never loads in
                the fleet.
              </p>
            </>
          )}
        </section>
      )}
    </div>
  );
}

function Tabs({ tab, read }: { tab: MemoryTab; read: MemoryResponse }) {
  const tabs: { id: MemoryTab; label: string; count: number }[] = [
    { id: "live", label: "Live", count: read.live.length },
    { id: "pending", label: "Pending", count: read.pending.length },
  ];
  return (
    <div className="mem-tabs" role="tablist" aria-label="Memory">
      {tabs.map((each) => (
        <Link
          key={each.id}
          id={`mem-tab-${each.id}`}
          to="/memory"
          search={(prev) => ({ ...prev, tab: each.id })}
          role="tab"
          aria-selected={tab === each.id}
          aria-controls={`mem-panel-${each.id}`}
          className="mem-tab"
        >
          {each.label}
          <span className="mem-cnt">{each.count}</span>
        </Link>
      ))}
    </div>
  );
}

function View({ project, role, tab }: { project?: string; role?: string; tab?: MemoryTab }) {
  const { data: read } = useSuspenseQuery(memoryQuery(project, role));
  const projects = useProjects();
  const to = useSearchTo();
  const shown: MemoryTab = tab ?? (read.pending.length > 0 ? "pending" : "live");
  const named = projects.find((each) => each.id === project);
  return (
    <>
      <header className="mem-head">
        <div className="mem-head-t">
          <h1>Memory</h1>
          <p className="mem-sub">
            {named ? named.name : "All projects"} · Notes live in kb. New ones wait in Pending until
            you keep them.
          </p>
        </div>
        <Select
          value={project ?? ALL}
          label="Project"
          className="mem-project"
          onValueChange={(next) => void to({ project: next === ALL ? undefined : next })}
          options={[
            { value: ALL, label: "All projects" },
            ...projects.map((each) => ({ value: each.id, label: each.name })),
          ]}
        />
      </header>
      <Tabs tab={shown} read={read} />
      {shown === "pending" ? <Pending read={read} projects={projects} /> : <Live read={read} />}
    </>
  );
}

export function MemoryPage(props: { project?: string; role?: string; tab?: MemoryTab }) {
  return (
    <div className="mem-root">
      <div className="mem-wrap">
        <Section label="Memory">
          <View {...props} />
        </Section>
      </div>
    </div>
  );
}
