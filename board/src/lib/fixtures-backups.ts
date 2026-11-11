import type { BackupList, BackupRow } from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

const FOLDER = "/home/user/.tiller/backups";
const MB = 1024 * 1024;

const verified = (id: string, kind: BackupRow["kind"], at: string, mb: number): BackupRow => ({
  id,
  kind,
  at,
  status: "verified",
  size: Math.round(mb * MB),
  error: null,
});

// What the stand-in server holds for a fresh page: a nightly and a hand-made
// copy, both verified. A `backup-failed` page also shows the attempt that did
// not verify; `backing-up` shows a run in progress; `empty` has none yet.
function held(scenario: Scenario): BackupRow[] {
  if (scenario === "empty" || scenario === "new-install") return [];
  const rows = [
    verified("20261110T030000000Z.db", "nightly", "2026-11-10T03:00:04.000Z", 48.3),
    verified("20261109T172211000Z.db", "manual", "2026-11-09T17:22:11.000Z", 47.1),
    verified("20261109T030000000Z.db", "nightly", "2026-11-09T03:00:03.000Z", 47),
  ];
  if (scenario === "backup-failed") {
    rows.unshift({
      id: "failed-1762790400000",
      kind: "manual",
      at: "2026-11-10T16:10:00.000Z",
      status: "failed",
      size: null,
      error: "the copy failed its integrity check",
    });
  }
  return rows;
}

// Backups taken from the board in this page's life, newest first.
let taken: BackupRow[] = [];
let count = 0;

// A new page life, for tests.
export function forgetTakenBackups() {
  taken = [];
}

export function backupsFor(scenario: Scenario): BackupList {
  return {
    folder: FOLDER,
    running: scenario === "backing-up",
    backups: [...taken, ...held(scenario)],
  };
}

// `POST /api/v1/backups`: a verified copy by hand, as the daemon makes one.
export function takeBackup(): BackupRow {
  count += 1;
  const at = new Date();
  const row = verified(`fixture-${count}.db`, "manual", at.toISOString(), 48.4);
  taken.unshift(row);
  return row;
}
