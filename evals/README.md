# Evals

Role behavior evals for the prompts in `roles/`. Each case gives one role a
small situation and grades what it does: the `tiller` commands it runs, the
files it leaves, and what it reports. The cases target behavior that goes
wrong silently, such as a builder that rebases a pushed branch, a reviewer
that fixes the bug it should report, or an orchestrator that approves a merge
nobody asked for. They never test wording or config values.

The evals run with `claude plugin eval --no-publish`. Tiller ships no skills
yet, so there are no skill-trigger evals and no per-skill no-skill baselines.
Both wait for the first skill.

## Layout

| Path | What it is |
|---|---|
| `cases/tuning/<case>/` | Cases you may read, run and tune the role prompts against |
| `cases/holdout/<case>/` | Sealed cases that score the prompts after tuning (see below) |
| `<case>/prompt.md` | The task. `role:` in the frontmatter names the role under test |
| `<case>/fixture.sh` | Optional. Builds the workspace: a git repo, canned `tiller` replies |
| `<case>/graders/*.md` | `tool_used`, `regex` and `llm` graders, in plugin eval format |
| `stub/tiller` | A stand-in `tiller` CLI that logs each call and answers from canned replies |
| `fixture-lib.sh` | `reply`, `repo` and `commit` helpers for `fixture.sh` |
| `scaffold.sh` | Prints a case's scaffold: the stub, the helpers, then `fixture.sh` |
| `pins.env` | The Claude Code version, the model each role alias resolves to, and the judge |
| `judge.lock` | The judge model and a hash of every `llm` rubric |
| `check.sh` | The offline checks. No model calls |
| `run.sh` | Builds a throwaway plugin from the cases and runs it |

`run.sh` launches each case the way `tiller spawn` launches the role: the
role's body is appended to the system prompt, and the role's model alias
resolves through `pins.env`. A case never sets a model or a system prompt of
its own.

## Infrastructure tests and evals stay separate

- `evals/check.sh` is the infrastructure test. It checks that every case
  names a real role and has a prompt and known graders, and that every
  fixture builds offline. It checks that no prompt appears in both splits,
  that every role has a case in each split, and that `judge.lock` is current.
  It makes no model calls, runs in about a second, and runs in CI on every
  push (the `evals` job in `ci.yml`).
- `evals/run.sh` runs the evals. It calls real models and costs money, so it
  runs only on demand. It runs `check.sh` first and refuses to start when
  that fails.

## Running

You need `claude` (the version in `pins.env`), `node`, `git` and `bash`. The
`evals` dev shell provides all four from the locked nixpkgs:

```sh
nix develop .#evals -c evals/run.sh --case 'reviewer_*' --runs 1
```

| Option | Meaning |
|---|---|
| `--split tuning\|holdout` | Which split runs. Default `tuning` |
| `--case GLOB` | Tuning only: the cases whose name matches |
| `--runs N` | Tuning only: runs per case. Default 3 |
| `--model M` | Tuning only: one model for every case, for a cheap check of the harness |
| `--max-cost-usd USD` | Starts no new runs once this much has been spent |
| `--out DIR` | Where the results go. Default: a new temp dir |
| `--threshold T` | Tuning only: the pass mark per case. Default 0.8 |
| `-j N` | Cases run at once. Default 1 |
| `--keep` | Tuning only: keep the work dir and each run's workspace and transcript |

Every Bash call inside a run goes through Claude Code's sandbox (bubblewrap
on Linux), confined to the run's workspace. Run `run.sh` from a plain
terminal, not from inside a sandboxed Claude Code session. A nested sandbox
cannot create its bridge sockets, and every Bash call fails. Transcripts show
a harmless `.bashrc: Permission denied` line from the sandboxed shell.

`check.sh --relock` rewrites `judge.lock` after a deliberate change to a
rubric or to the judge.

## The holdout

The holdout measures whether a prompt change generalizes. It stops working
the moment anyone tunes against it, so:

- Never read `cases/holdout/` while editing a role prompt or a tuning case.
  When a tuning case needs a new idea, write a new tuning case.
- `run.sh --split holdout` always runs the whole split at each case's own
  model and run count, at the default pass mark. It refuses `--case`,
  `--runs`, `--model`, `--threshold` and `--keep`, and it needs the pinned
  Claude Code version.
- It prints only the aggregate: cases passed, overall score and cost. The
  per-case results land under `<out>/sealed/`. Don't open them while tuning.
- Run it once a tuning round is finished, not as a step inside one. A failing
  holdout means "write more tuning cases", not "fix this case".
- A holdout case may change only when it is wrong as a test (a broken
  fixture, a grader that misreads correct behavior), never to raise a score.
  Record the reason in the commit.

## Pins

What a score depends on is pinned, so any change shows up as a commit:

- `pins.env` holds the Claude Code version, the model ID for each role alias
  (`opus`, `fable`, `sonnet`) and the judge model. `run.sh` warns when the
  installed `claude` differs from the pin, and stops in CI or on the holdout. The `evals` dev shell gets Claude
  Code from the locked nixpkgs, so update the pin together with `flake.lock`.
- `judge.lock` holds the judge model and the sha256 of every `llm` rubric.
  `check.sh` fails when either one drifts.
- The judge is a different model from the one under test, so no role grades
  itself.

Change one pin at a time. A pin change re-baselines both splits.

## CI

The `Evals` workflow (`.github/workflows/evals.yml`) runs only when started by
hand from the Actions tab, with a split and a cost cap. It never runs on push
and deploys nothing.

The evals use the owner's Claude subscription, not API pricing. Locally,
`claude` uses its existing login; unset `ANTHROPIC_API_KEY` first, because a
key takes precedence over the login and bills at API prices. In CI the
workflow reads the repository secret `CLAUDE_CODE_OAUTH_TOKEN`, fails at once
when it is missing, and unsets `ANTHROPIC_API_KEY` in the job. To set it up:

1. Run `claude setup-token` and sign in. It prints a long-lived token.
2. Run `gh secret set CLAUDE_CODE_OAUTH_TOKEN -R jrnxf/tiller` and paste it.

`--max-cost-usd` still works on a subscription: `claude plugin eval` counts
the estimated API-equivalent cost of each run and stops with
`partial: cost_ceiling` at the cap. The dollars are not billed; they measure
how much of the subscription's usage a run consumes.

Each run uploads only `summary.txt`: per-case scores for tuning, the
aggregate for the holdout. Transcripts stay on the runner, because an agent
can print its environment, which holds the token. To read transcripts, run
tuning locally with `--keep`.

## Writing a case

1. Make `cases/tuning/<snake_case_name>/` with a `prompt.md`. The prompt is
   what the role would receive: a card, an inbox line, a run brief.
2. Add `fixture.sh` when the role needs a workspace. Use `repo` and `commit`
   for git state and `reply KEY [EXIT]` for what `tiller` answers. For
   example, `reply spawn-alpha-44 1` makes `tiller spawn alpha-44 ...` fail.
   Fixtures run offline, and their values follow the repo rules
   (`example.com`, `/home/user`).
3. Add graders. Prefer `tool_used` and `regex`, which are deterministic. Use
   an `llm` grader only for what they cannot see, then run
   `evals/check.sh --relock`.
4. Run it on the tuning split until it passes for the right reason. Read the
   kept transcript (`--keep`).
5. Give the holdout a case of the same kind with a different situation, and
   don't tune against it.
