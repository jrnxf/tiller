import { expect, test } from "vitest";

import type { PollerHealth } from "../../lib/api-types";
import { doctorFor, mergeAppFor, peerDoctorFor, pollersFor } from "../../lib/fixture-doctor";
import { checkLines, mergeAppProblem, needsYou, peerCheckLines, pollerTitle } from "./check-lines";
import type { Words } from "./check-lines";

const NOW = Date.parse("2026-11-11T10:00:00Z");

const text = (words: Words | undefined) =>
  (words ?? []).map((word) => (typeof word === "string" ? word : word.code)).join("");

test("a report that passes is one quiet line per topic", () => {
  const lines = checkLines(doctorFor("federated", NOW), pollersFor("federated", NOW));
  const tools = lines.find((line) => line.key === "tools:found");
  expect(tools?.sub).toContain("git");
  const pollers = lines.find((line) => line.key === "pollers");
  expect(pollers?.tone).toBe("green");
  expect(pollers?.sub).toBe("12 running");
  expect(lines.filter((line) => line.title === "Pollers")).toHaveLength(1);
});

test("a missing tool is a hint to add it through Nix, a whole sentence after its error", () => {
  const lines = checkLines(doctorFor("federated", NOW), pollersFor("federated", NOW));
  const missing = lines.find((line) => line.key === "tool:gh-axi");
  expect(missing?.sub).toBe("gh-axi missing");
  expect(missing?.tone).toBe("amber");
  expect(missing?.end).toEqual({ kind: "note", text: "Hint only" });
  // The error ends in ")": the sentence still closes before the hint.
  expect(text(missing?.detail)).toBe(
    "Cannot run gh-axi: No such file or directory (os error 2). Add it to the host's Nix config, then check again.",
  );
});

test("a poller that keeps failing says how often and since when, and can be run now", () => {
  const lines = checkLines(doctorFor("default", NOW), pollersFor("default", NOW));
  const reconcile = lines.find((line) => line.key === "poller:reconcile");
  expect(reconcile?.title).toBe("Reconcile poller");
  expect(reconcile?.tone).toBe("red");
  expect(text(reconcile?.detail)).toMatch(
    /^5 errors in a row\. Last read OK Wed 11 Nov, 09:13\. It keeps trying on its own\./,
  );
  expect(reconcile?.end?.kind).toBe("run");
  expect(needsYou(lines)).toEqual({ tone: "red", text: "1 check needs you" });
});

test("a poller Run now brought back reads as fixed, and the count drops", () => {
  const failing = pollersFor("default", NOW);
  const after: PollerHealth = {
    source: "reconcile",
    last_attempt_at: new Date(NOW).toISOString(),
    last_success_at: new Date(NOW).toISOString(),
    consecutive_failures: 0,
    last_error: null,
  };
  const lines = checkLines(doctorFor("default", NOW), failing, { reconcile: after });
  const reconcile = lines.find((line) => line.key === "poller:reconcile");
  expect(reconcile?.tone).toBe("green");
  expect(reconcile?.end).toEqual({ kind: "note", text: "Fixed" });
  expect(text(reconcile?.detail)).toBe("Running again since 10:00. First read OK.");
  expect(needsYou(lines)).toBeNull();
});

test("the check names of the other system never reach the board", () => {
  const lines = checkLines(doctorFor("default", NOW), pollersFor("default", NOW));
  const all = lines.map((line) => `${line.title} ${line.sub ?? ""} ${text(line.detail)}`);
  expect(all.join("\n")).not.toMatch(/firstmate/i);
});

test("the merge App and the merge gates are the Merge App section's, not the host card's", () => {
  const lines = checkLines(doctorFor("default", NOW), pollersFor("default", NOW));
  expect(lines.some((line) => line.key === "merge-gates")).toBe(false);
  expect(lines.some((line) => line.key === "check:merge app")).toBe(false);
});

test("the Merge App section's problems are one line for the count, red when the key has leaked", () => {
  const gates = doctorFor("default", NOW).merge_gates;
  const missing = mergeAppFor("default");
  expect(mergeAppProblem(missing, gates)).toEqual([
    { key: "merge-app", tone: "amber", title: "Merge App" },
  ]);
  expect(mergeAppProblem({ ...missing, status: "fail" }, gates)[0]?.tone).toBe("red");
  // A ready key with a repo still on detect is still one problem.
  const ready = mergeAppFor("merge-ready");
  expect(mergeAppProblem(ready, doctorFor("merge-ready", NOW).merge_gates)).toHaveLength(1);
  const enforced = doctorFor("merge-ready", NOW).merge_gates.filter(
    (gate) => gate.merge_gate === "enforced",
  );
  expect(mergeAppProblem(ready, enforced)).toEqual([]);
  // Unread health and no gates: nothing to count.
  expect(mergeAppProblem(undefined, [])).toEqual([]);
});

test("a peer's card has its own merge App and gates, grouped by verdict", () => {
  const report = peerDoctorFor("host-d", NOW);
  if (!report) throw new Error("no fixture report for host-d");
  const lines = peerCheckLines(report);
  expect(lines.find((line) => line.key === "check:merge app")?.title).toBe("Merge App");
  const gates = lines.find((line) => line.key === "merge-gates");
  expect(gates?.tone).toBe("green");
  expect(text(gates?.detail)).toBe("gamma, fleet (enforced): the ruleset check holds.");
  // A peer's pollers live in its daemon: none come over the link.
  expect(lines.some((line) => line.key.startsWith("poller"))).toBe(false);
  expect(peerDoctorFor("host-e", NOW)).toBeUndefined();
});

test.each([
  ["herdr:local", "herdr poller"],
  ["herdr:host-b", "herdr poller for host-b"],
  ["merge-gate", "Merge gate poller"],
])("the %s poller is called the %s", (source, title) => {
  expect(pollerTitle(source)).toBe(title);
});
