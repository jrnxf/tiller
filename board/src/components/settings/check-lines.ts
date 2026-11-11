import type {
  CheckStatus,
  DoctorCheck,
  DoctorGate,
  DoctorReport,
  MergeAppHealth,
  PollerHealth,
} from "../../lib/api-types";

// The host card's check lines, worded for the board from the report `tiller
// doctor` makes (`GET /api/v1/doctor`) and the pollers `/api/v1/health`
// lists. One line per topic: a passing topic is one quiet line, a problem
// says what is wrong and, where the board can act, carries the fix.

export type Tone = "green" | "amber" | "red";

// A run of text where a command or a file name shows as code.
export type Words = (string | { code: string })[];

type LineEnd =
  | { kind: "note"; text: string }
  // A failing poller the owner can poll again at once.
  | { kind: "run"; poller: PollerHealth };

export interface CheckLine {
  key: string;
  tone: Tone;
  title: string;
  sub?: string;
  detail?: Words;
  end?: LineEnd;
}

// Always with the weekday: "Thu 5 Nov, 23:40".
const dayTime = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "short",
  day: "numeric",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

const clock = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

export function whenText(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? null : dayTime.format(at);
}

function clockText(iso: string | null): string {
  if (!iso) return "now";
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? "now" : clock.format(at);
}

const TONE: Record<CheckStatus, Tone> = { ok: "green", warn: "amber", fail: "red" };

const worst = (tones: Tone[]): Tone =>
  tones.includes("red") ? "red" : tones.includes("amber") ? "amber" : "green";

const OK: LineEnd = { kind: "note", text: "OK" };

// The checks that are tools on PATH, in the report's order.
const TOOLS = new Set([
  "git",
  "treehouse",
  "herdr",
  "claude",
  "gh",
  "gh-axi",
  "quota-axi",
  "no-mistakes",
  "no-mistakes daemon",
]);

const NIX: Words = [" Add it to the host's Nix config, then check again."];

function toolLines(tools: DoctorCheck[]): CheckLine[] {
  if (tools.length === 0) return [];
  const found = tools.filter((check) => check.status === "ok");
  const problems = tools.filter((check) => check.status !== "ok");
  const names = found.map((check) => check.name).join(", ");
  if (problems.length === 0) {
    return [
      {
        key: "tools",
        tone: "green",
        title: "Tools",
        sub: names,
        end: { kind: "note", text: "All found" },
      },
    ];
  }
  const lines: CheckLine[] = problems.map((check) => {
    const missing = !check.version;
    const fix: Words =
      check.name === "herdr"
        ? [" Start herdr, or add it to the host's Nix config, then check again."]
        : missing && check.name !== "no-mistakes daemon"
          ? NIX
          : [];
    return {
      key: `tool:${check.name}`,
      tone: TONE[check.status],
      title: "Tools",
      sub: missing ? `${check.name} missing` : check.name,
      detail: [sentence(check.detail), ...fix],
      end: { kind: "note", text: "Hint only" },
    };
  });
  if (found.length > 0) {
    lines.push({
      key: "tools:found",
      tone: "green",
      title: "Other tools",
      sub: names,
      end: { kind: "note", text: "Found" },
    });
  }
  return lines;
}

// "herdr:local" is this host's herdr; "herdr:host-b" reads another machine's.
export function pollerTitle(source: string): string {
  if (source === "herdr:local") return "herdr poller";
  if (source.startsWith("herdr:")) return `herdr poller for ${source.slice("herdr:".length)}`;
  const words = source.replace(/[-_]/g, " ");
  return `${words.charAt(0).toUpperCase()}${words.slice(1)} poller`;
}

function pollerLines(pollers: PollerHealth[], fixed: Record<string, PollerHealth>): CheckLine[] {
  const lines: CheckLine[] = [];
  const failing = pollers.filter(
    (poller) => poller.consecutive_failures > 0 && !fixed[poller.source],
  );
  for (const poller of failing) {
    const count = poller.consecutive_failures;
    const lastOk = whenText(poller.last_success_at);
    const detail: Words = [
      `${count} ${count === 1 ? "error" : "errors"} in a row. `,
      lastOk ? `Last read OK ${lastOk}.` : "No read has worked since the daemon started.",
      " It keeps trying on its own.",
    ];
    if (poller.last_error) detail.push(" Last error: ", { code: poller.last_error });
    lines.push({
      key: `poller:${poller.source}`,
      tone: count >= 3 ? "red" : "amber",
      title: pollerTitle(poller.source),
      detail,
      end: { kind: "run", poller },
    });
  }
  for (const poller of Object.values(fixed)) {
    lines.push({
      key: `poller:${poller.source}`,
      tone: "green",
      title: pollerTitle(poller.source),
      detail: [`Running again since ${clockText(poller.last_success_at)}. First read OK.`],
      end: { kind: "note", text: "Fixed" },
    });
  }
  const fine = pollers.filter(
    (poller) => poller.consecutive_failures === 0 && !fixed[poller.source],
  );
  if (fine.length > 0) {
    // ISO times in UTC sort as text.
    const newest = fine.reduce<string | null>(
      (latest, poller) =>
        poller.last_success_at && (!latest || poller.last_success_at > latest)
          ? poller.last_success_at
          : latest,
      null,
    );
    const read = whenText(newest);
    lines.push({
      key: "pollers",
      tone: "green",
      title: failing.length > 0 || Object.keys(fixed).length > 0 ? "Other pollers" : "Pollers",
      sub: `${fine.length} running`,
      detail: [read ? `No errors. Last read ${read}.` : "No errors."],
      end: OK,
    });
  }
  return lines;
}

// The owner marker and a second dispatching system, worded by the report's
// code so the board names no other product.
const OWNER: Record<string, { tone?: Tone; detail: Words }> = {
  tiller: { detail: ["Tiller owns this host."] },
  unset: {
    detail: [
      "No owner is named, so Tiller dispatches here. Name the owner with ",
      { code: "tiller owner set <owner>" },
      ".",
    ],
  },
  other_system: {
    detail: ["Another system owns this host, and the Tiller daemon is not running."],
  },
  other_system_daemon_running: {
    detail: [
      "Another system owns this host, but the Tiller daemon runs here. Stop the daemon, or hand the host over with ",
      { code: "tiller owner set tiller" },
      ".",
    ],
  },
  named_other: { detail: ["The owner marker names another owner, so Tiller is read only here."] },
};

const OTHER_SYSTEM: Record<string, { detail: Words }> = {
  not_running: { detail: ["No other dispatching system runs here."] },
  processes_unreadable: {
    detail: ["The process list cannot be read, so no other dispatching system was looked for."],
  },
  both_dispatch: {
    detail: [
      "Another dispatching system runs here, and Tiller would also dispatch, so both could start the same work. Stop one of them, or name the owner with ",
      { code: "tiller owner set <owner>" },
      ".",
    ],
  },
  kept_read_only: {
    detail: ["Another system runs here, and the owner marker keeps Tiller from dispatching."],
  },
};

// The Merge App section above the hosts shows this check and the merge gates
// (merge-app.tsx), so this host's card does not say it again. A peer's card
// does (`peerCheckLines`).
const MERGE_APP = "merge app";

const TITLES: Record<string, string> = {
  "hook registration": "Hooks",
  "owner marker": "Owner marker",
  firstmate: "Other system",
  config: "Config",
  "config drift": "Config drift",
  state: "State",
};

// The report's details are lower-case clauses; a line reads as a sentence.
export function sentence(text: string): string {
  const trimmed = text.trim();
  if (!trimmed) return trimmed;
  // A file or tool name keeps its case: "config.toml", "gh".
  const first = trimmed.split(/\s/, 1)[0] ?? "";
  const word = /^[a-z]+$/.test(first) && !TOOLS.has(first);
  const head = word ? `${trimmed.charAt(0).toUpperCase()}${trimmed.slice(1)}` : trimmed;
  return /[.!?]$/.test(head) ? head : `${head}.`;
}

function checkLine(check: DoctorCheck): CheckLine {
  const tone = TONE[check.status];
  const title = TITLES[check.name] ?? sentence(check.name).replace(/\.$/, "");
  const worded =
    check.name === "owner marker"
      ? OWNER[check.code ?? ""]
      : check.name === "firstmate"
        ? OTHER_SYSTEM[check.code ?? ""]
        : undefined;
  // A daemon newer than this board may send a code it does not know; it still
  // must not print the other system's name.
  const other = check.name === "owner marker" || check.name === "firstmate";
  const detail: Words = worded
    ? worded.detail
    : other
      ? ["Run the checks in a shell for this line's full text."]
      : [sentence(check.detail)];
  return {
    key: `check:${check.name}`,
    tone,
    title,
    detail,
    ...(tone === "green" ? { end: OK } : {}),
  };
}

export function checkLines(
  report: DoctorReport | undefined,
  pollers: PollerHealth[],
  fixed: Record<string, PollerHealth> = {},
): CheckLine[] {
  const checks = report?.checks ?? [];
  const lines = [
    ...toolLines(checks.filter((check) => TOOLS.has(check.name))),
    ...pollerLines(pollers, fixed),
    ...checks.filter((check) => !TOOLS.has(check.name) && check.name !== MERGE_APP).map(checkLine),
  ];
  return lines;
}

// A peer's merge gates in its card, one clause per gate and verdict: "alpha,
// beta (detect): detection only; ...". This board's own gates are in the
// Merge App section instead.
function gateLine(gates: DoctorGate[]): CheckLine | null {
  if (gates.length === 0) return null;
  const tone = worst(gates.map((gate) => TONE[gate.status]));
  const groups = new Map<string, { gate: string; text: string; projects: string[] }>();
  for (const gate of gates) {
    const text = gate.detail.replace(/^merge gate:\s*/, "");
    const group = groups.get(`${gate.merge_gate}\n${text}`);
    if (group) group.projects.push(gate.project);
    else
      groups.set(`${gate.merge_gate}\n${text}`, {
        gate: gate.merge_gate,
        text,
        projects: [gate.project],
      });
  }
  const detail: Words = [...groups.values()].map(
    ({ gate, text, projects }, index) =>
      `${index > 0 ? " " : ""}${projects.join(", ")} (${gate}): ${text}${/[.!?]$/.test(text) ? "" : "."}`,
  );
  return {
    key: "merge-gates",
    tone,
    title: "Merge gates",
    sub: `${gates.length} ${gates.length === 1 ? "project" : "projects"}`,
    detail,
    ...(tone === "green" ? { end: OK } : {}),
  };
}

// A paired peer's checks, read from it and shown read-only. Its pollers live
// in that daemon, so the report is all there is. The Merge App section on
// this board is this host's, so the peer's merge App key and gates show in
// the peer's card.
export function peerCheckLines(report: DoctorReport): CheckLine[] {
  const app = report.checks.find((check) => check.name === MERGE_APP);
  const gates = gateLine(report.merge_gates);
  return [
    ...checkLines(report, []),
    ...(app ? [{ ...checkLine(app), title: "Merge App" }] : []),
    ...(gates ? [gates] : []),
  ];
}

// The Merge App section's problems (merge-app.tsx), counted once for the
// host row: the host's key and every gate on detect are one topic, as the
// section is one. The card does not show it again.
export function mergeAppProblem(app: MergeAppHealth | undefined, gates: DoctorGate[]): CheckLine[] {
  const key: Tone[] = !app || app.status === "ok" ? [] : [app.status === "fail" ? "red" : "amber"];
  const tone = worst([...key, ...gates.map((gate) => TONE[gate.status])]);
  return tone === "green" ? [] : [{ key: "merge-app", tone, title: "Merge App" }];
}

// What the host row's status says when its card holds problems: "2 checks
// need you", red when one has failed. Null when every line passes.
export function needsYou(lines: CheckLine[]): { tone: Tone; text: string } | null {
  const open = lines.filter((line) => line.tone !== "green");
  if (open.length === 0) return null;
  return {
    tone: worst(open.map((line) => line.tone)),
    text: `${open.length} ${open.length === 1 ? "check needs" : "checks need"} you`,
  };
}
