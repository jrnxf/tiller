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
import { sentence, whenText } from "./check-lines";
import type { Tone } from "./check-lines";
import { CardLine, HostCardSection } from "./host-card";
import "./settings.css";

// GitHub owns these steps, so the board only links to the right page. It
// reads the checks and changes nothing on GitHub, and the key never passes
// through it.
const NEW_APP = "https://github.com/settings/apps/new";
const INSTALLATIONS = "https://github.com/settings/installations";
const rulesets = (repo: string) => `https://github.com/${repo}/settings/rules`;

// Step 4 of the setup doc, for the owner's own terminal. The daemon reads
// `<home>/forge-app`. Health has no field for that folder, but its detail
// names it (or a file in it), so the commands use that folder when the
// detail has one. Otherwise they take the home from `$TILLER_HOME` with the
// default `~/.tiller`, as the daemon does. Health never has the App ID while
// the commands show, so the ID and the download are placeholders: bash and
// zsh refuse the `<APP_ID>` line, and `mv` finds no file by the placeholder
// name, so neither line changes anything until the owner replaces it.
const SET_ID = 'echo <APP_ID> > "$dir/app-id"';
const SET_KEY = [
  'mv "<DOWNLOADED_KEY.pem>" "$dir/private-key.pem"',
  'chmod 600 "$dir/private-key.pem"',
];
const SETUP = ['mkdir -p "$dir"', 'chmod 700 "$dir"', SET_ID, ...SET_KEY];
const PRIVATE = ['chmod 600 "$dir/private-key.pem"'];
const LOOK = ['ls -la "$dir"'];

const HOME_NOTE =
  "The commands use $TILLER_HOME when your shell sets it, and the default ~/.tiller when it does not. Check that it is the home the daemon uses.";
const PLACEHOLDER_NOTE =
  "Replace <APP_ID> with your App ID, and <DOWNLOADED_KEY.pem> with the full path of the key GitHub downloaded, starting with / or $HOME (a ~ does not expand inside the quotes). Until you do, those lines fail and change nothing.";
const ROTATE_NOTE =
  "If anyone else could have read the key, rotate it: on GitHub, Settings, Developer settings, GitHub Apps, open the App, generate a new key, replace private-key.pem (mode 600), then delete the old key.";
const ACCESS_NOTE =
  "Tiller cannot open the file, so writing it again would not help. Check its owner and mode: it must be yours, and the key must be a file with mode 600.";

// What health's detail says, read from the daemon's own wording
// (`merge_app_health` and `KeyError`): the App folder, and on a `warn` the
// file Tiller cannot use and why. A folder is read only when it is absolute
// and the wording is known; anything else is null, and the board falls
// back to `$TILLER_HOME`.
interface KeyDetail {
  dir: string | null;
  file: "app-id" | "key" | null;
  why: string | null;
}

function readDetail(detail: string): KeyDetail {
  const error =
    /detect: (\/.*\/forge-app)\/(app-id|private-key\.pem)(?:: (.*)| is readable by group or others)/.exec(
      detail,
    );
  if (error) {
    return {
      dir: error[1] ?? null,
      file: error[2] === "app-id" ? "app-id" : "key",
      why: error[3] ?? null,
    };
  }
  const named = / in (\/.*\/forge-app)(?: \(|$)/.exec(detail);
  return { dir: named?.[1] ?? null, file: null, why: null };
}

// Whether a key file is on the host, so the App exists: an exposed key, an
// `app-id` problem (Tiller reads it only after the key file passes), or a
// key file whose contents are not a key.
function keyOnHost(app: MergeAppHealth): boolean {
  if (app.status === "ok" || app.status === "fail") return true;
  if (app.status !== "warn") return false;
  const { file, why } = readDetail(app.detail);
  return file === "app-id" || (file === "key" && /^not a (PEM|usable)/.test(why ?? ""));
}

// One shell word, quoted as it is.
const quoted = (text: string) => `'${text.replaceAll("'", `'\\''`)}'`;

// What the owner runs for each key status, and what to read first. An
// exposed key needs only its mode. A `warn` names the file Tiller cannot
// use: an `app-id` that is missing or not a number gets its line, a key
// that is not a key gets the key lines, and a file Tiller cannot open gets
// a look at the folder, since writing it again would not help.
function keyFix(app: MergeAppHealth): { lines: string[]; notes: string[] } {
  const { dir, file, why } = readDetail(app.detail);
  const head = dir ? [`dir=${quoted(dir)}`] : ['dir="${TILLER_HOME:-$HOME/.tiller}/forge-app"'];
  const home = dir ? [] : [HOME_NOTE];
  if (app.status === "fail") return { lines: [...head, ...PRIVATE], notes: [...home, ROTATE_NOTE] };
  const notes = [...home, PLACEHOLDER_NOTE];
  if (app.status !== "warn") return { lines: [...head, ...SETUP], notes };
  const blocked = why !== null && (/os error/.test(why) || why === "not a file");
  const absent = why !== null && /os error 2\)/.test(why);
  if (file === "app-id" && (absent || !blocked)) return { lines: [...head, SET_ID], notes };
  if (file === "key" && !blocked) return { lines: [...head, ...SET_KEY], notes };
  return { lines: [...head, ...LOOK], notes: [...home, ACCESS_NOTE] };
}

const STATUS_TEXT: Record<MergeAppHealth["status"], string> = {
  ok: "Ready",
  missing: "Not set up",
  warn: "Key not used",
  fail: "Key exposed",
};

const PLACE = "Place the key and the App ID";

const KEY_TITLE: Record<MergeAppHealth["status"], string> = {
  ok: PLACE,
  missing: PLACE,
  warn: "Fix the key or the App ID",
  fail: "Make the key private",
};

// Every link reads "Open on GitHub", and its name says which page.
function GitHubLink({ href, label }: { href: string; label: string }) {
  return (
    <a
      className="btn sm"
      href={href}
      target="_blank"
      rel="noreferrer"
      aria-label={`Open on GitHub: ${label}`}
    >
      <ExternalIcon />
      Open on GitHub
    </a>
  );
}

// A done step is a green dot and "Done". A step to do is amber (red for an
// exposed key) with its fix on the right.
function Step({
  testid,
  tone,
  title,
  detail,
  end,
  extra,
}: {
  testid: string;
  tone: Tone;
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

// Step 1 of the doc. Health sees only the key, and a key file on the host,
// usable or not, means the App exists. When Tiller cannot even open the
// folder or the file, it cannot tell.
function AppStep({ app, host }: { app: MergeAppHealth; host: string | null }) {
  const missing = !keyOnHost(app);
  const tone = app.status === "ok" ? "ok" : app.status === "fail" ? "fail" : "missing";
  return (
    <HostCardSection
      title={host ?? "This host"}
      meta={
        <span className={`set-status ${tone}`} role="status" data-testid="merge-app-status">
          {STATUS_TEXT[app.status]}
        </span>
      }
    >
      <ul className="set-lines">
        <Step
          testid="merge-step-app"
          tone={missing ? "amber" : "green"}
          title="Create the App"
          detail={
            missing
              ? `${app.status === "warn" ? "Tiller cannot open the key, so it cannot tell whether the App exists. If it does not: " : ""}Name it tiller-merge. Give it Contents, Pull requests and Workflows read and write, Checks read-only, and no webhook.`
              : app.status === "ok" && app.app_id !== null
                ? `App ${app.app_id} has its key on this host.`
                : "A key for the App is on this host."
          }
          end={<GitHubLink href={NEW_APP} label="new App" />}
        />
      </ul>
    </HostCardSection>
  );
}

// Step 4 of the doc, last as the doc has it: the key goes in after the
// rulesets exist.
function KeyStep({ app, host }: { app: MergeAppHealth; host: string | null }) {
  const [shown, setShown] = useState(false);
  const ready = app.status === "ok";
  const fix = shown && !ready ? keyFix(app) : null;
  const last = app.status === "missing" ? " Do this last, after each repo's ruleset exists." : "";
  return (
    <HostCardSection title={`The key on ${host ?? "this host"}`}>
      <ul className="set-lines">
        <Step
          testid="merge-step-key"
          tone={ready ? "green" : app.status === "fail" ? "red" : "amber"}
          title={KEY_TITLE[app.status]}
          detail={
            ready
              ? "The key is in place and only you can read it."
              : `${sentence(app.detail)}${last} Run the commands in your own terminal, not in an agent.`
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
            fix ? (
              <div className="set-line-x">
                {fix.notes.map((note) => (
                  <p key={note}>{note}</p>
                ))}
                <pre className="set-line-c" data-testid="merge-commands">
                  {fix.lines.join("\n")}
                </pre>
              </div>
            ) : null
          }
        />
      </ul>
    </HostCardSection>
  );
}

// Steps 2 and 3 of the doc, for one gated repo. A repo on detect is missing
// the install or the ruleset, and the check does not say which, so both stay
// open and the install step asks the owner to check, not to install.
function RepoSteps({
  gate,
  repo,
  app,
}: {
  gate: DoctorGate;
  repo: string | undefined;
  app: MergeAppHealth;
}) {
  const enforced = gate.merge_gate === "enforced";
  const changed = whenText(gate.last_changed_at);
  const name = repo ?? gate.project;
  const until =
    app.status === "ok"
      ? "Merges are detected, not prevented, until it holds."
      : `${app.status === "missing" ? "Add it before the key. " : ""}The daemon checks it only once the key works, and until then merges are detected, not prevented.`;
  return (
    <HostCardSection
      title={repo ? `github.com/${repo}` : gate.project}
      meta={
        <span className={`set-status ${enforced ? "ok" : "missing"}`}>
          {enforced ? "Enforced" : "Detect"}
        </span>
      }
    >
      <ul className="set-lines" data-testid={`merge-repo-${gate.project}`}>
        <Step
          testid={`merge-step-install-${gate.project}`}
          tone={enforced ? "green" : "amber"}
          title="App installed on the repo"
          detail={
            enforced
              ? undefined
              : "Check that the App is installed on this repo. If it is not: Install App, then Only select repositories, and pick this repo."
          }
          end={<GitHubLink href={INSTALLATIONS} label={`install the App on ${name}`} />}
        />
        <Step
          testid={`merge-step-ruleset-${gate.project}`}
          tone={enforced ? "green" : "amber"}
          title="Ruleset holds"
          detail={
            enforced
              ? `${changed ? `Enforced since ${changed}. ` : ""}The daemon checks again about every hour.`
              : `Default branch. Only the App bypasses. Pull request, status checks, no force push. ${until}`
          }
          end={repo ? <GitHubLink href={rulesets(repo)} label={`rulesets of ${repo}`} /> : null}
        />
      </ul>
    </HostCardSection>
  );
}

// Settings > Merge App: the steps that make a repo's merge gate hold, in the
// setup doc's order: create the App, then each gated repo's install and
// ruleset, then the host's key, last. It reads what `tiller doctor` reads
// (the host's merge App key from health, each project's gate from the doctor
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
  return (
    <section className="set-sec" aria-label="Merge App" data-testid="merge-app">
      <div className="set-sec-h">
        <div>
          <h2>Merge App</h2>
          <p>The App is the only identity that may merge to a gated repo's default branch.</p>
        </div>
        {/* Nothing refetches health on its own, so one Check again reads the
            key and every repo again. */}
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
      </div>
      <div className="set-card">
        <AppStep app={app} host={host} />
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
          />
        ))}
        <KeyStep app={app} host={host} />
      </div>
    </section>
  );
}
