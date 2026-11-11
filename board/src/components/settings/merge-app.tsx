import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { ReactNode } from "react";

import type { DoctorGate, MergeAppHealth } from "../../lib/api-types";
import {
  doctorQuery,
  healthQuery,
  projectSetupPath,
  projectSetupQuery,
  queryKeyFor,
} from "../../lib/queries";
import { ExternalIcon, RetryIcon } from "../board/icons";
import { whenText } from "./check-lines";
import type { Tone } from "./check-lines";
import { CardLine, HostCardSection } from "./host-card";
import "./settings.css";

// GitHub owns these steps, so the board only links to the right page. It
// reads the checks and changes nothing on GitHub, and the key never passes
// through it.
const NEW_APP = "https://github.com/settings/apps/new";
const INSTALLATIONS = "https://github.com/settings/installations";
const rulesets = (repo: string) => `https://github.com/${repo}/settings/rules`;

// Step 4 of the setup doc, for the owner's own terminal.
const KEY_COMMANDS = [
  "mkdir -p ~/.tiller/forge-app",
  "chmod 700 ~/.tiller/forge-app",
  "echo 1234567 > ~/.tiller/forge-app/app-id",
  "mv /path/to/tiller-merge.*.private-key.pem ~/.tiller/forge-app/private-key.pem",
  "chmod 600 ~/.tiller/forge-app/private-key.pem",
].join("\n");

const STATUS_TEXT: Record<MergeAppHealth["status"], string> = {
  ok: "Ready",
  missing: "Not set up",
  warn: "Key not used",
  fail: "Key exposed",
};

function GitHubLink({ href }: { href: string }) {
  return (
    <a className="btn sm" href={href} target="_blank" rel="noreferrer">
      <ExternalIcon />
      Open on GitHub
    </a>
  );
}

// A done step is a green dot and "Done". A step to do is amber (red for an
// exposed key) with its fix on the right. A step that waits on an earlier one
// is a plain dot.
function Step({
  testid,
  tone,
  title,
  detail,
  end,
  extra,
}: {
  testid: string;
  tone: Tone | null;
  title: string;
  detail?: string;
  end?: ReactNode;
  extra?: ReactNode;
}) {
  return (
    <CardLine
      testid={testid}
      tone={tone}
      title={title}
      detail={detail ? [detail] : undefined}
      end={tone === "green" ? <span className="set-line-r">Done</span> : end}
      extra={extra}
    />
  );
}

function HostSteps({ app, host }: { app: MergeAppHealth; host: string | null }) {
  const [shown, setShown] = useState(false);
  const ready = app.status === "ok";
  return (
    <HostCardSection
      title={host ?? "This host"}
      meta={
        <span
          className={`set-status ${ready ? "ok" : app.status === "fail" ? "fail" : "missing"}`}
          data-testid="merge-app-status"
        >
          {STATUS_TEXT[app.status]}
        </span>
      }
    >
      <ul className="set-lines">
        <Step
          testid="merge-step-app"
          tone={ready ? "green" : "amber"}
          title="Create the App"
          detail={
            ready
              ? app.app_id === null
                ? "The App has a key."
                : `App ${app.app_id}, with the permissions Tiller needs`
              : "Name it tiller-merge. Give it Contents, Pull requests and Workflows read and write, and no webhook."
          }
          end={<GitHubLink href={NEW_APP} />}
        />
        <Step
          testid="merge-step-key"
          tone={ready ? "green" : app.status === "fail" ? "red" : "amber"}
          title="Place the key and the App ID"
          detail={
            ready
              ? "The key is in place and only you can read it."
              : `${app.detail} Run the commands in your own terminal, not in an agent.`
          }
          end={
            <button
              type="button"
              className="btn sm"
              aria-expanded={shown}
              onClick={() => setShown(!shown)}
            >
              {shown ? "Hide commands" : "Show commands"}
            </button>
          }
          extra={
            shown && !ready ? (
              <pre className="set-line-c" data-testid="merge-commands">
                {KEY_COMMANDS}
              </pre>
            ) : null
          }
        />
      </ul>
    </HostCardSection>
  );
}

function RepoSteps({
  gate,
  repo,
  app,
  action,
}: {
  gate: DoctorGate;
  repo: string | undefined;
  app: MergeAppHealth;
  action: ReactNode;
}) {
  const enforced = gate.merge_gate === "enforced";
  const changed = whenText(gate.last_changed_at);
  // Until the host's App is ready the ruleset step waits on it. After that, a
  // repo on detect is missing the install or the ruleset, and the check does
  // not say which, so both stay open.
  const waits = app.status !== "ok";
  return (
    <HostCardSection
      title={repo ? `github.com/${repo}` : gate.project}
      meta={
        <span className={`set-status ${enforced ? "ok" : "missing"}`}>
          {enforced ? "Enforced" : "Detect"}
        </span>
      }
      action={enforced ? null : action}
    >
      <ul className="set-lines" data-testid={`merge-repo-${gate.project}`}>
        <Step
          testid={`merge-step-install-${gate.project}`}
          tone={enforced ? "green" : "amber"}
          title="App installed on the repo"
          detail={
            enforced ? undefined : "Install App, then Only select repositories, and pick this repo."
          }
          end={<GitHubLink href={INSTALLATIONS} />}
        />
        <Step
          testid={`merge-step-ruleset-${gate.project}`}
          tone={enforced ? "green" : waits ? null : "amber"}
          title="Ruleset holds"
          detail={
            enforced
              ? `${changed ? `Enforced since ${changed}. ` : ""}The daemon checks again about every hour.`
              : "Default branch. Only the App bypasses. Pull request, status checks, no force push. Merges are detected, not prevented, until it holds."
          }
          end={repo ? <GitHubLink href={rulesets(repo)} /> : null}
        />
      </ul>
    </HostCardSection>
  );
}

// Settings > Merge App: the steps that make a repo's merge gate hold, for the
// host and for each gated repo. It reads what `tiller doctor` reads (the
// host's merge App key from health, each project's gate from the doctor
// report) and links to the GitHub page for each step GitHub owns.
export function MergeAppSection({ app, host }: { app: MergeAppHealth; host: string | null }) {
  const client = useQueryClient();
  const doctor = useQuery(doctorQuery());
  const gates = doctor.data?.merge_gates ?? [];
  const setups = useQueries({ queries: gates.map((gate) => projectSetupQuery(gate.project)) });
  const again = () => {
    void doctor.refetch();
    void client.invalidateQueries({ queryKey: healthQuery().queryKey });
    for (const gate of gates) {
      void client.invalidateQueries({ queryKey: queryKeyFor(projectSetupPath(gate.project)) });
    }
  };
  const checkAgain = (
    <button
      type="button"
      className="btn sm"
      onClick={again}
      disabled={doctor.isFetching}
      aria-busy={doctor.isFetching || undefined}
    >
      <RetryIcon />
      Check again
    </button>
  );
  return (
    <section className="set-sec" aria-label="Merge App" data-testid="merge-app">
      <div className="set-sec-h">
        <div>
          <h2>Merge App</h2>
          <p>The App is the only identity that may merge to a gated repo's default branch.</p>
        </div>
      </div>
      <div className="set-card set-merge">
        <HostSteps app={app} host={host} />
        {doctor.isError ? (
          <HostCardSection title="Repos">
            <ul className="set-lines">
              <CardLine
                testid="merge-repos-unreadable"
                tone="amber"
                title="Repos"
                detail={[`They cannot be read from here: ${doctor.error.message}`]}
              />
            </ul>
          </HostCardSection>
        ) : null}
        {gates.map((gate, index) => (
          <RepoSteps
            key={gate.project}
            gate={gate}
            repo={setups[index]?.data?.repo ?? undefined}
            app={app}
            action={checkAgain}
          />
        ))}
      </div>
    </section>
  );
}
