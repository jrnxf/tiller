import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";

import type { Project } from "../../lib/api-types";
import { ownerOf } from "../../lib/peers";
import { projectsPath, queryKeyFor } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { InlineReceipt } from "../attention/receipt";
import { useAction } from "../attention/use-action";
import { lisbonDay, lisbonShortDay } from "../health/lisbon";
import { DeleteProject } from "./project-delete";
import "./project-stage.css";

type Stage = Project["stage"];

// What the owner is told before the switch (docs/design/pre-launch-validation.md,
// sections 3 and 7).
const CONFIRM: Record<Stage, string> = {
  "pre-launch":
    "Per-change validation (no-mistakes) is skipped and a batched review runs every Saturday at 06:00 Lisbon time. Grants skip their pipeline condition for a PR with no pipeline run while pre-launch; every other condition still applies.",
  launched:
    "Validation is back on for every push from now on. A final review of everything since the last reviewed commit starts next. Open PRs with no pipeline run must go back through the gate before they merge.",
};

// "Sat 7 Nov 06:00", always Lisbon time, always with the weekday.
function formatStageTime(iso: string | null): string | null {
  if (!iso || Number.isNaN(Date.parse(iso))) return null;
  return `${lisbonShortDay(iso)} ${lisbonDay(iso).time}`;
}

// The first twelve characters of a commit hash.
function sha12(sha: string): string {
  return sha.slice(0, 12);
}

// The sidebar chip: pre-launch always, "final review" while a launch review
// runs, nothing for an ordinary launched project.
export function StageChip({ project }: { project: Project }) {
  if (project.stage === "pre-launch") {
    return (
      <span className="stage-chip" data-stage="pre-launch">
        pre-launch
      </span>
    );
  }
  if (project.review_open === "final") {
    return (
      <span className="stage-chip" data-stage="final">
        final review
      </span>
    );
  }
  return null;
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="stage-fact">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

function Facts({ project }: { project: Project }) {
  const prelaunch = project.stage === "pre-launch";
  const reviewedAt = formatStageTime(project.reviewed_at ?? project.stage_changed_at);
  const next = formatStageTime(project.next_review_due);
  return (
    <dl className="stage-facts">
      {prelaunch ? (
        <>
          <Fact label="Last reviewed commit">
            {project.reviewed_through_sha ? (
              <>
                <code className="stage-sha">{sha12(project.reviewed_through_sha)}</code>
                {reviewedAt ? <span className="stage-when">{reviewedAt}</span> : null}
              </>
            ) : (
              "none yet"
            )}
          </Fact>
          <Fact label="Next review due">
            {next ?? "not scheduled"}
            {project.review_overdue ? <span className="stage-flag">review overdue</span> : null}
          </Fact>
        </>
      ) : project.review_open === "final" ? (
        <Fact label="Launch review">
          <span className="stage-note">final review running</span>
        </Fact>
      ) : null}
      <Fact label="Builders follow">
        <code className="stage-sha">{project.effective_delivery}</code>
      </Fact>
    </dl>
  );
}

// The confirm step of a stage switch. It is a modal on the page itself, not
// inside the sidebar, which is a transformed drawer on a phone.
function ConfirmStage({
  project,
  target,
  onClose,
  onRecorded,
}: {
  project: Project;
  target: Stage;
  onClose: () => void;
  onRecorded: (receiptKey: string, text: string) => void;
}) {
  const queryClient = useQueryClient();
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/projects/${project.id}/stage`, {
        rev: project.rev,
        stage: target,
        idempotency_key,
      }),
    [project.id, project.rev, target],
  );
  const recorded = useCallback(
    (_: unknown, key: string) => {
      onRecorded(key, `Switched to ${target}`);
      onClose();
    },
    [onClose, onRecorded, target],
  );
  // The `rev` is part of the click: a project that changed is a new decision.
  const action = useAction({
    binding: `${project.id}:${project.rev}:${target}`,
    send,
    onRecorded: recorded,
  });
  const { error } = action;
  const stale = error?.status === 409;
  useEffect(() => {
    // A stale `rev` means the board read an older project; read it again so
    // the next try carries the current one.
    if (stale) void queryClient.invalidateQueries({ queryKey: queryKeyFor(projectsPath) });
  }, [queryClient, stale]);

  const cancel = useRef<HTMLButtonElement>(null);
  const confirm = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancel.current?.focus();
  }, []);

  const heading = `Switch ${project.name} to ${target}?`;
  return createPortal(
    <div
      className="dialog-backdrop"
      onMouseDown={() => {
        // Like Escape: a pending switch keeps its dialog, so a refusal shows.
        if (!action.pending) onClose();
      }}
    >
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="stage-dialog-title"
        aria-describedby="stage-dialog-body"
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!action.pending) onClose();
          } else if (event.key === "Tab") {
            // Two buttons are the only stops; keep focus between them.
            const [first, last] = [cancel.current, confirm.current];
            if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last?.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first?.focus();
            }
          }
        }}
      >
        <h2 id="stage-dialog-title">{heading}</h2>
        <p id="stage-dialog-body">{CONFIRM[target]}</p>
        {error ? (
          <p className="refusal" role="alert">
            {stale
              ? `${error.message} The project was read again, so confirm once more to switch it.`
              : error.message}
          </p>
        ) : null}
        <div className="dialog-actions">
          <button
            ref={cancel}
            type="button"
            className="btn"
            disabled={action.pending}
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            ref={confirm}
            type="button"
            className="btn primary"
            disabled={action.pending}
            onClick={action.trigger}
          >
            {action.pending ? "Switching..." : `Switch to ${target}`}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

// What opens under a project row: its stage facts, the stage switch and the
// delete.
export function ProjectDetail({ project, id }: { project: Project; id: string }) {
  const [confirming, setConfirming] = useState(false);
  const [receipt, setReceipt] = useState<{ key: string; text: string } | null>(null);
  const opener = useRef<HTMLButtonElement>(null);
  const target: Stage = project.stage === "pre-launch" ? "launched" : "pre-launch";
  // The board host does not forward a stage switch (protocol.md section 7.4).
  const owner = ownerOf(project);
  const off = owner ? `${owner} owns this project: switch its stage there` : undefined;

  const close = useCallback(() => {
    setConfirming(false);
    opener.current?.focus();
  }, []);

  const onRecorded = useCallback((key: string, text: string) => setReceipt({ key, text }), []);

  return (
    <div className="pdetail" id={id} role="group" aria-label={`${project.name} stage`}>
      <Facts project={project} />
      <button
        ref={opener}
        type="button"
        className="btn stage-switch"
        disabled={off !== undefined}
        title={off}
        onClick={() => setConfirming(true)}
      >
        Switch to {target}
      </button>
      {receipt ? (
        <div className="stage-receipt">
          <InlineReceipt receiptKey={receipt.key} text={receipt.text} />
        </div>
      ) : null}
      {confirming ? (
        <ConfirmStage project={project} target={target} onClose={close} onRecorded={onRecorded} />
      ) : null}
      <DeleteProject project={project} owner={owner} />
    </div>
  );
}
