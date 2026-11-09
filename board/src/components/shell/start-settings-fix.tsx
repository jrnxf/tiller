import { useCallback, useState } from "react";

import type { SettingProblem } from "../../lib/api-types";
import type { ActionError } from "../attention/api";
import { SettingField } from "../settings/setting-field";
import "./ask-dialog.css";

const LABELS: Record<string, string> = {
  "spawn.pool_root": "Worktree pool",
  "spawn.checkouts_root": "Checkouts folder",
};

const HINTS: Record<string, string> = {
  "spawn.pool_root": "Where the worktrees agents work in are kept",
  "spawn.checkouts_root": "Where each project's checkout lives, as <folder>/<project>",
};

// Every setting a refused start lists, or none when the refusal was about
// something else. The daemon sends them in `current.settings` (protocol.md
// section 6.4).
export function settingsOf(error: ActionError): SettingProblem[] {
  const listed = error.current?.settings;
  if (!Array.isArray(listed)) return [];
  return listed.filter(
    (entry): entry is SettingProblem =>
      typeof entry === "object" &&
      entry !== null &&
      typeof (entry as SettingProblem).key === "string" &&
      typeof (entry as SettingProblem).problem === "string",
  );
}

// A start that failed on settings names all of them at once. On this host each
// takes a path and a Save; on a peer each is set on that host, so the board
// shows its command. Retry stays the caller's own button.
export function StartSettingsFix({
  problems,
  project,
  host,
  checkoutsRoot,
}: {
  problems: SettingProblem[];
  // The project's id, for the default checkout folder.
  project: string;
  // The peer the start ran on; absent for this host.
  host: string | undefined;
  // The saved checkouts folder, when this host has one.
  checkoutsRoot: string | null;
}) {
  const [saved, setSaved] = useState<Set<string>>(new Set());
  const done = useCallback((key: string) => setSaved((now) => new Set(now).add(key)), []);
  const all = problems.every((problem) => saved.has(problem.key));
  return (
    <div className="ask-fix" data-testid="start-settings-fix">
      <p className="ask-fix-title">
        {problems.length === 1
          ? "One setting is missing or wrong"
          : `${problems.length} settings are missing or wrong`}
        {host ? ` on ${host}` : ""}
      </p>
      {problems.map((problem) => {
        const checkout = problem.key.startsWith("checkouts.");
        return (
          <SettingField
            key={problem.key}
            settingKey={problem.key}
            label={checkout ? "Checkout folder" : (LABELS[problem.key] ?? problem.key)}
            hint={checkout ? "The project's clone on this host" : (HINTS[problem.key] ?? "")}
            value={null}
            suggestion={checkout && checkoutsRoot ? `${checkoutsRoot}/${project}` : undefined}
            problem={problem.kind === "invalid" ? problem.problem : null}
            host={host}
            onSaved={() => done(problem.key)}
          />
        );
      })}
      {!host && all ? (
        <p className="ask-fix-done" role="status">
          Saved. Press Retry to send.
        </p>
      ) : null}
    </div>
  );
}
