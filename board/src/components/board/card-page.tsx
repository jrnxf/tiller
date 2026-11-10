import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import type { CSSProperties, ReactNode } from "react";

import type {
  AttentionItem,
  CardApproval,
  CardArtifact,
  CardPr,
  CardRun,
  TimelineEntry,
  TimelineResponse,
} from "../../lib/api-types";
import { HOST_PROJECT, focusReason, hostName, ownerOf, shownId } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import {
  cardQuery,
  fetchJson,
  machinesQuery,
  projectsQuery,
  timelinePath,
  timelineQuery,
} from "../../lib/queries";
import { agentActivity } from "./activity";
import { Ago } from "./ago";
import { stateText, statusMark } from "./badge";
import { CardHeader } from "./card-header";
import { bugProjectOf, isTillerBug, isUntriaged, machineOf } from "./bugs";
import { HostChip, PeerNote } from "./host-chip";
import { ApproveActions } from "../attention/approve";
import { phaseOf } from "../attention/card-lifecycle";
import { AnswerForm } from "../attention/answer";
import { CommentControl } from "../attention/card-actions";
import "../attention/inbox.css";
import {
  useAmbiguousIds,
  useHostLabel,
  useMachineRows,
  useNamedProjects,
  useStanding,
} from "./peer-state";
import { approvalFor, approvalStanding, mergeTimeline } from "./card-model";
import { durationSeconds, formatAge, formatLisbonMoment, secondsSince, shortSha } from "./format";
import { focusTooltip, useHerdrFocus, useOpenKey } from "./herdr";
import { LiveTerminal, TerminalToggle, useTerminalOpen } from "./live-terminal";
import {
  BoltIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ExpandIcon,
  PaneIcon,
  PRIORITY_NAMES,
  PrIcon,
  Priority,
  RouteIcon,
  SendIcon,
  Status,
  UserIcon,
  WarnIcon,
} from "./icons";
import { KindLabel, ModelChip } from "./tile";
import { BugTriage } from "./triage";
import { CheckIcon, CloseIcon } from "../shell/icons";
import "./board.css";

const ACTOR_LABEL = { you: "You", orchestrator: "Orchestrator", hook: "Hook", system: "Tiller" };

function ActorIcon({ actor }: { actor: TimelineEntry["actor"] }) {
  if (actor === "you") return <UserIcon />;
  if (actor === "hook") return <BoltIcon />;
  if (actor === "orchestrator") return <SendIcon />;
  return <PaneIcon />;
}

function Timeline({
  cardId,
  owner,
  composer,
}: {
  cardId: string;
  owner: string | undefined;
  composer: ReactNode;
}) {
  const { data: first } = useSuspenseQuery(timelineQuery(cardId, owner));
  // Earlier pages belong to the first page they were loaded under. When an event
  // refetches the first page, the pages below it may no longer join it, so they
  // are dropped and "Show earlier" starts again from the new first page.
  const [loaded, setLoaded] = useState<{
    base: unknown;
    older: TimelineEntry[];
    cursor: number | null | undefined;
  }>({ base: first, older: [], cursor: undefined });
  const [failed, setFailed] = useState(false);
  let { older, cursor } = loaded;
  if (loaded.base !== first) {
    older = [];
    cursor = undefined;
    setLoaded({ base: first, older, cursor });
  }
  const next = cursor === undefined ? first.next_before : cursor;
  const entries = mergeTimeline(first.entries, older);

  const loadEarlier = async () => {
    if (next === null) return;
    setFailed(false);
    try {
      const page = await fetchJson<TimelineResponse>(timelinePath(cardId, next, owner));
      setLoaded((current) =>
        current.base === first
          ? {
              base: first,
              older: mergeTimeline(current.older, page.entries),
              cursor: page.next_before,
            }
          : current,
      );
    } catch {
      setFailed(true);
    }
  };

  return (
    <div className="sect">
      <div className="sect-h">
        Timeline<span className="c">{entries.length} newest first</span>
      </div>
      {composer}
      {entries.length === 0 ? (
        <div className="run">
          <p style={{ margin: 0 }}>Nothing has happened on this card yet.</p>
        </div>
      ) : (
        <div className="tl">
          {entries.map((entry) => (
            <div key={entry.seq} className="it">
              <span className="ic">
                <ActorIcon actor={entry.actor} />
              </span>
              <span className="tx">
                <b>{ACTOR_LABEL[entry.actor]}</b> <span className="q">{entry.text}</span>
              </span>
              <span className="when">{formatLisbonMoment(entry.at)}</span>
            </div>
          ))}
        </div>
      )}
      {next === null ? null : (
        <button type="button" className="btn ghost sm" onClick={() => void loadEarlier()}>
          Show earlier
        </button>
      )}
      {failed ? <p className="muted">Could not load earlier events. Try again.</p> : null}
    </div>
  );
}

function RunCard({ run, owner }: { run: CardRun; owner: string | undefined }) {
  const off = focusReason(owner);
  const focus = useHerdrFocus();
  const { data: machines } = useSuspenseQuery(machinesQuery());
  const failed = run.outcome === "failed";
  const active = run.ended_at === null;
  const activity = run.activity
    ? agentActivity({
        activity: run.activity,
        activity_observed_at: run.activity_observed_at ?? "",
        role: run.role,
      })
    : null;
  const seconds = durationSeconds(run.started_at, run.ended_at);
  const machine = machines.machines.find((candidate) => candidate.id === run.host_id);
  const sessionId = run.session_id;
  const hostId = run.host_id;
  const remote = machine?.kind === "remote";
  const terminalOpen = useTerminalOpen();
  // Only the owner sees an agent's terminal: live for this machine's agents,
  // a read-only tail for a saved machine's.
  const mirror = active && sessionId && hostId && off === null;
  const openPane = (
    <button
      type="button"
      className="btn sm"
      disabled={off !== null}
      title={off ?? focusTooltip(machine)}
      onClick={() => sessionId && hostId && focus({ session_id: sessionId, host_id: hostId })}
    >
      <PaneIcon />
      Open in herdr
    </button>
  );
  return (
    <div className={`run${failed ? " fail" : ""}`}>
      <div className="rh">
        <Status
          mark={
            failed ? "fail" : active ? "working" : run.outcome === "cancelled" ? "retired" : "ok"
          }
        />
        Attempt {run.number}
        <span className="lab sm">{run.role}</span>
        {failed ? <span className="lab sm bad">failed</span> : null}
        {run.outcome === "cancelled" ? <span className="lab sm">cancelled</span> : null}
        {run.outcome === "passed" ? <span className="lab sm ok">passed</span> : null}
        {active ? <span className="lab sm yel">running</span> : null}
        <span className="sp" />
        <ModelChip model={run.model} />
      </div>
      <p>
        {formatLisbonMoment(run.started_at)}
        {run.ended_at ? ` to ${formatLisbonMoment(run.ended_at)}` : ""}
        {seconds === null ? "" : `, ${formatAge(seconds)}`}.{run.note ? ` ${run.note}` : ""}
      </p>
      {active && sessionId && hostId ? (
        <div className="acts right">
          {activity ? (
            <span className="meta">
              <i className={`ld ${activity.dot}`} /> {activity.label}
            </span>
          ) : null}
          {mirror ? <TerminalToggle size="sm" /> : null}
          {openPane}
        </div>
      ) : null}
      {mirror && terminalOpen ? (
        <LiveTerminal
          sessionId={sessionId}
          label={`${run.role} attempt ${run.number}`}
          heading={
            <span className="mir-title">
              Attempt {run.number}
              <span className="lab sm">{run.role}</span>
              {machine ? <span className="meta">{machine.label}</span> : null}
            </span>
          }
          actions={openPane}
          machine={remote ? machine.label : undefined}
        />
      ) : null}
    </div>
  );
}

// The pull request and its approval are one card: what is being approved on the
// left, what you can do about it on the right.
function PullRequest({
  pr,
  approvals,
  attention,
}: {
  pr: CardPr;
  approvals: CardApproval[];
  attention: AttentionItem[];
}) {
  const approval = approvalFor(approvals, pr);
  const standing = approval ? approvalStanding(approval, pr) : null;
  const stale = standing === "stale";
  const checkTone = pr.checks === "passing" ? "ok" : pr.checks === "failing" ? "bad" : "yel";
  const who = approval?.decided_by === "owner" ? "you" : (approval?.decided_by ?? "someone");
  return (
    <div className="sect">
      <div className="sect-h">
        Pull request
        <a className="c" href={pr.url} target="_blank" rel="noreferrer">
          {pr.repo}#{pr.number}
        </a>
      </div>
      <div className={`prbox prc${stale ? " stale" : ""}`} data-standing={standing ?? "none"}>
        <div className="ph1">
          <PrIcon />
          {pr.title}
          <span className="sp" />
          <span
            className={`lab sm ${pr.state === "merged" ? "acc" : pr.state === "open" ? "ok" : ""}`}
          >
            {pr.state}
          </span>
        </div>
        <div className="prc-body">
          <dl>
            <dt>Head</dt>
            <dd>
              <span className={`sha${stale ? " new" : ""}`}>{shortSha(pr.head_sha)}</span>
              {stale && approval ? (
                <span className="meta">
                  +{approval.newer_commits.length} commit
                  {approval.newer_commits.length === 1 ? "" : "s"} since approval
                </span>
              ) : null}
            </dd>
            <dt>Base</dt>
            <dd className="mono">{pr.base}</dd>
            <dt>Checks</dt>
            <dd>
              <span className={`lab sm ${checkTone}`}>
                {pr.checks === "passing" ? <CheckIcon /> : null}
                {pr.checks_summary ?? pr.checks}
              </span>
            </dd>
            {pr.diff_summary ? (
              <>
                <dt>Diff</dt>
                <dd>{pr.diff_summary}</dd>
              </>
            ) : null}
            <dt>Approval</dt>
            <dd>
              {!approval ? (
                <span className="meta">
                  {pr.state === "open" ? "None yet. One is requested once checks pass." : "None."}
                </span>
              ) : standing === "pending" ? (
                <span className="lab sm yel">waiting for you</span>
              ) : standing === "current" ? (
                <>
                  <span className="lab sm ok">
                    <CheckIcon />
                    approved
                  </span>
                  <span className="meta">
                    by {who}
                    {approval.decided_at ? `, ${formatLisbonMoment(approval.decided_at)}` : ""},
                    bound to <span className="sha">{shortSha(approval.head_sha ?? "")}</span>
                  </span>
                </>
              ) : stale ? (
                <>
                  <span className="lab sm yel">stale</span>
                  <span className="meta">
                    bound to <span className="sha old">{shortSha(approval.head_sha ?? "")}</span>
                    <ChevronRightIcon />
                    head is <span className="sha new">{shortSha(pr.head_sha)}</span>
                  </span>
                </>
              ) : (
                <span className="lab sm">{approval.state}</span>
              )}
            </dd>
          </dl>
          {approval && standing === "pending" ? (
            <ApproveActions
              item={attention.find(
                (item) => item.still_current && item.approval?.id === approval.id,
              )}
            />
          ) : null}
        </div>
        {stale && approval && approval.newer_commits.length > 0 ? (
          <ul className="prc-commits">
            {approval.newer_commits.map((commit) => (
              <li key={commit.sha}>
                <span className="mono">{shortSha(commit.sha)}</span>
                <span>{commit.subject}</span>
                <span className="meta" style={{ marginLeft: "auto" }}>
                  {formatLisbonMoment(commit.at)}
                </span>
              </li>
            ))}
          </ul>
        ) : null}
      </div>
    </div>
  );
}

// What the host asks of you now, each answered in place.
function Alerts({ alerts }: { alerts: AttentionItem[] }) {
  return (
    <div className="sect">
      <div className="sect-h">
        Open alerts<span className="c">{alerts.length} waiting</span>
      </div>
      {alerts.length > 0 ? (
        alerts.map((item) => (
          <div key={item.id} className="run">
            <p style={{ margin: 0 }}>
              <b>{item.question}</b>
            </p>
            <AnswerForm item={item} />
          </div>
        ))
      ) : (
        <div className="run">
          <p style={{ margin: 0 }}>No open alerts.</p>
        </div>
      )}
    </div>
  );
}

// Pages, screenshots and reports made for the card, so each stays findable
// after the conversation that made it is gone.
function Pages({ artifacts }: { artifacts: CardArtifact[] }) {
  return (
    <div className="sect" data-testid="card-pages">
      <div className="sect-h">
        Pages<span className="c">{artifacts.length} published</span>
      </div>
      {artifacts.length > 0 ? (
        artifacts.map((artifact) => (
          <div key={artifact.id} className="run">
            <div className="rh">
              <span className="lab sm">{artifact.kind}</span>
              {artifact.url ? (
                <a href={artifact.url} target="_blank" rel="noreferrer">
                  {artifact.title}
                </a>
              ) : (
                artifact.title
              )}
              <span className="sp" />
              <span className="meta">{formatLisbonMoment(artifact.created_at)}</span>
            </div>
          </div>
        ))
      ) : (
        <div className="run">
          <p style={{ margin: 0 }}>No pages have been made for this card.</p>
        </div>
      )}
    </div>
  );
}

export function CardPage({
  cardId,
  owner,
  variant = "page",
}: {
  cardId: string;
  owner?: string;
  // The drawer is the same card, docked beside the board.
  variant?: "page" | "drawer";
}) {
  const { data: detail } = useSuspenseQuery(cardQuery(cardId, owner));
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const { card, column } = detail;
  const standing = useStanding(column);
  const id = shownId(column.host ? { ...card, host: column.host } : card, useAmbiguousIds());
  // A peer's project is the one that host owns, whatever slug this host uses.
  const owned = (p: { id: string; host?: string }) =>
    p.id === card.project_id && (p.host ?? null) === (ownerOf(column) ?? null);
  const project =
    projects.projects.find(owned) ?? projects.projects.find((p) => p.id === card.project_id);
  const color = projectColor(project ? projects.projects.indexOf(project) : -1);
  const focus = useHerdrFocus();
  const activeRun = detail.runs.find((run) => run.ended_at === null);
  const sessionId = activeRun?.session_id;
  const hostId = activeRun?.host_id;
  useOpenKey(
    sessionId && hostId && !ownerOf(column)
      ? () => focus({ session_id: sessionId, host_id: hostId })
      : null,
  );

  // Per PR, from the approval that binds it: an old approval that was replaced
  // must not raise the banner while the new one is current.
  let staleApproval: CardApproval | null = null;
  let stalePr: CardPr | undefined;
  for (const pr of detail.prs) {
    const approval = approvalFor(detail.approvals, pr);
    if (approval && approvalStanding(approval, pr) === "stale") {
      staleApproval = approval;
      stalePr = pr;
      break;
    }
  }

  // The standing Host health card: no orchestrator reads its comments and no
  // run, PR or page ever lands on it, so it shows its open alerts instead.
  const hostHealth = card.project_id === HOST_PROJECT && card.kind === "ops";
  const alerts = detail.attention.filter(
    (item) => item.still_current && item.answered_at === null && item.withdrawn_at === null,
  );

  const machines = useMachineRows();
  const hostLabel = useHostLabel(column);
  const bug = isTillerBug(column);
  const untriaged = isUntriaged(column);
  // A project id on this card's host, as the board names it.
  const projectName = (projectId: string) =>
    projectId === HOST_PROJECT
      ? hostName(hostLabel)
      : (projects.projects.find(
          (p) => p.id === projectId && (p.host ?? null) === (ownerOf(column) ?? null),
        )?.name ?? projectId);
  const bugProject = bugProjectOf(machineOf(column, machines), projects.projects);

  const hold = detail.holds[0];
  const phase = phaseOf(column);
  const blockers = detail.deps
    .filter((dep) => dep.released_at === null)
    .map((dep) => dep.display_id);

  return (
    <div className={`card-page${variant === "drawer" ? " in-drawer" : ""}`} data-testid="card-page">
      <div className="cp-nav">
        {variant === "page" ? (
          <Link
            to="/"
            search={(prev) => ({ host: prev.host, card: cardId, owner })}
            className="btn ghost sm"
          >
            <ChevronLeftIcon />
            Board
          </Link>
        ) : null}
        <span className="pdot" style={{ "--pc": color } as CSSProperties} />
        <span className="muted">{project?.name ?? card.project_id}</span>
        <ChevronRightIcon />
        <b>{id}</b>
        <HostChip entity={column} />
        {variant === "drawer" ? (
          <span className="cp-tools">
            <Link
              to="/cards/$cardId"
              params={{ cardId }}
              search={{ owner }}
              className="ib sm"
              title="Open the full page"
              aria-label="Open the full page"
            >
              <ExpandIcon />
            </Link>
            <Link
              to="/"
              search={(prev) => ({ ...prev, card: undefined, owner: undefined })}
              className="ib sm"
              title="Close"
              aria-label="Close"
            >
              <CloseIcon />
            </Link>
          </span>
        ) : null}
      </div>
      <PeerNote standing={standing} />

      {staleApproval && stalePr ? (
        <div className="pk-banner">
          <WarnIcon />
          <div>
            <b>Approval is stale.</b> It was bound to{" "}
            <span className="sha old">{shortSha(staleApproval.head_sha)}</span> and the head is now{" "}
            <span className="sha new">{shortSha(stalePr.head_sha)}</span>. Merging needs a fresh
            approval.
          </div>
        </div>
      ) : null}

      <CardHeader
        detail={detail}
        off={standing.canAct ? null : standing.reason}
        actions={!hostHealth}
      >
        {untriaged ? (
          <BugTriage
            row={column}
            body={card.body ?? null}
            projects={projects.projects}
            standing={standing}
          />
        ) : null}
        <dl className="props">
          <dt>Status</dt>
          <dd>
            <Status mark={statusMark(column)} />
            {stateText(column, blockers)}
            {untriaged ? <span className="lab sm yel">Untriaged</span> : null}
            {blockers.length > 0 && column.badge !== "blocked" ? (
              <span className="meta">blocked by {blockers.join(", ")}</span>
            ) : null}
          </dd>
          {hostHealth || phase === "closed" ? (
            <>
              <dt>Priority</dt>
              <dd>
                <span className="pri">
                  <Priority level={card.priority} />
                </span>
                {PRIORITY_NAMES[card.priority] ?? "None"}
              </dd>
            </>
          ) : null}
          <dt>Kind</dt>
          <dd>
            <KindLabel kind={card.kind} />
            {bug ? <span className="lab sm bad">Bug</span> : null}
          </dd>
          {bug ? (
            <>
              {column.filed_from ? (
                <>
                  <dt>Filed from</dt>
                  <dd>{projectName(column.filed_from)}</dd>
                </>
              ) : null}
              <dt>Route</dt>
              <dd>
                <span className="mono">tiller bug</span>
                <span className="route-arrow">
                  <RouteIcon />
                </span>
                {projectName(card.project_id)}
                {card.project_id === HOST_PROJECT ? (
                  <span className="meta">
                    {bugProject
                      ? `filed before ${bugProject.name} took Tiller bugs`
                      : "no bug project here"}
                  </span>
                ) : null}
              </dd>
            </>
          ) : null}
          <dt>Model</dt>
          <dd>
            {card.model ? (
              <ModelChip model={card.model} />
            ) : (
              <span className="muted">none yet</span>
            )}
          </dd>
          <dt>Slug</dt>
          <dd>
            <span className="mono">{card.slug}</span>
          </dd>
          {card.branch ? (
            <>
              <dt>Branch</dt>
              <dd>
                <span className="mono">{card.branch}</span>
              </dd>
            </>
          ) : null}
          {card.worktree ? (
            <>
              <dt>Worktree</dt>
              <dd>
                <span className="mono">{card.worktree}</span>
              </dd>
            </>
          ) : null}
        </dl>
      </CardHeader>

      {hostHealth ? <Alerts alerts={alerts} /> : null}

      <Timeline
        cardId={cardId}
        owner={owner}
        composer={
          hostHealth ? null : (
            <CommentControl card={column} off={standing.canAct ? null : standing.reason} />
          )
        }
      />

      {hostHealth ? null : (
        <>
          <div className="sect">
            <div className="sect-h">
              Runs
              <span className="c">
                {detail.runs.length} attempt{detail.runs.length === 1 ? "" : "s"}
              </span>
            </div>
            {detail.runs.length > 0 ? (
              detail.runs.map((run) => <RunCard key={run.id} run={run} owner={ownerOf(column)} />)
            ) : (
              <div className="run">
                <p style={{ margin: 0 }}>
                  No runs yet. The orchestrator starts one when the card leaves{" "}
                  {hold ? "its hold" : "the queue"}.
                </p>
              </div>
            )}
          </div>

          {detail.prs.length > 0 ? (
            detail.prs.map((pr) => (
              <PullRequest
                key={pr.id}
                pr={pr}
                approvals={detail.approvals}
                attention={detail.attention}
              />
            ))
          ) : (
            <div className="sect">
              <div className="sect-h">Pull request</div>
              <div className="run">
                <p style={{ margin: 0 }}>
                  No PR yet. An approval is requested once one is open, and it is bound to the head
                  commit at that moment.
                </p>
              </div>
            </div>
          )}

          <Pages artifacts={detail.artifacts} />
        </>
      )}

      <div className="sect">
        <div className="sect-h">
          Events<span className="c">{detail.events.length} newest</span>
        </div>
        {detail.events.length > 0 ? (
          <div className="evlog">
            {detail.events.map((event) => (
              <div
                key={`${event.at}-${event.source}-${event.text}`}
                className={event.tone === "bad" ? "bad" : event.tone === "warn" ? "warnl" : ""}
              >
                <span className="tm">{formatLisbonMoment(event.at)}</span>
                <span className="src">{event.source}</span>
                <span className="tx">{event.text}</span>
              </div>
            ))}
          </div>
        ) : (
          <p className="muted">No events yet.</p>
        )}
      </div>

      {hostHealth ? null : (
        <div className="sect">
          <div className="sect-h">
            Observation freshness<span className="c">how recent each signal is</span>
          </div>
          <div className="fresh">
            {detail.freshness.map((item) => {
              const age = secondsSince(item.observed_at);
              const kind =
                age === null ? "idle" : age < 30 ? "live" : age >= 300 ? "stale" : "idle";
              return (
                <div key={item.source} className="fr">
                  <i className={`ld ${kind}`} />
                  <div>
                    <b>{item.source}</b>
                    <small>{item.detail}</small>
                  </div>
                  <span className={`t${age !== null && age >= 300 ? " old" : ""}`}>
                    {age === null ? "n/a" : <Ago at={item.observed_at} prefix="observed" />}
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
