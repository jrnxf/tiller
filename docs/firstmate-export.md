# Export a Firstmate home into a Tiller import bundle

Tiller starts on day one with no history of its own, so everything a Firstmate home knows reaches it through one import bundle. `scripts/firstmate-export` writes that bundle. It is a script, not a prompt: it copies records verbatim, makes no model call, and lists what only a person can settle in `open-questions.md`.

It needs Node 20 or newer (the Nix dev shell has it) and `git`, and nothing else.

## Run it

```sh
scripts/firstmate-export \
  --home <the Firstmate home> \
  --out "$HOME/.tiller/import/<label>-$(TZ=Europe/Lisbon date +%Y%m%d-%H%M)-dry" \
  --host-label <short label for this machine> \
  --mode dry
```

| Option | Meaning |
|---|---|
| `--home` | The Firstmate home to read. Also read from `FM_HOME`. |
| `--out` | A new, empty directory outside the home. Every run uses a fresh one. |
| `--mode` | `dry` is a rehearsal any time. `final` is only for cutover, after dispatch is frozen. Default `dry`. |
| `--host-label` | The label the importer records for this host. Default: the short hostname. |
| `--auto-memory` | The Claude Code auto-memory directory for this home, or `none`. Default: the directory Claude Code derives from the home's path. |
| `--project-alias old=new` | Cards that name project `old` go to project `new`. Use it for a renamed project. |
| `--repo-alias old/name=new/name` | PR links in repo `old/name` are rewritten to `new/name`. Use it for a renamed GitHub repo. |
| `--allow-secret-hit path` | Allow secret-scan hits under this bundle path (see below). Repeatable. |

Exit codes: 0 the bundle is written; 1 usage error or not a Firstmate home; 3 the secret scan found a hit nobody allowed.

Then read `README.md`, `open-questions.md` and `secret-scan.md` in the bundle, and check it with the importer, which reads nothing it cannot use and changes nothing under `--dry-run`:

```sh
tiller import --dry-run <bundle>
```

The bundle is local only. It holds verbatim records and transcripts, so never commit it, push it or attach it anywhere. If the home serves an employer's work, the bundle stays on that machine.

## What the script reads and writes

The script is read-only on the home. It opens files for reading, and its only commands there are `git config --get remote.origin.url`, `git symbolic-ref` and `git rev-parse`, each run only in a directory that holds its own `.git`. A plain directory under the home or under another repository never reports that repository's remote. Every write goes into the bundle directory. The test `scripts/firstmate-export-check.sh` proves the home is byte-identical after a run.

It reads `data/projects.md`, `data/backlog.md`, `data/done-archive.md`, `data/note-archive.md`, `data/captain.md`, `data/learnings.md`, `data/memory-archive.md`, the auto-memory directory, `data/<task-id>/`, `config/`, and the per-task `state/<task-id>.meta` and `.status` files. It never opens a secret-bearing file by design: names like `.env*`, `*.pem`, `*.key` and `credentials` are skipped before they are read.

## The bundle (schema `tiller-import/v1`)

```
<out>/
  manifest.json          # written last; sha256 of every other file, and the counts
  README.md              # the counts, what was skipped, what the script did not write
  projects.json
  cards.jsonl
  memory.jsonl
  memory/rules.md, notes.md, requirements.md   # rendered from memory.jsonl
  memory-guesses.tsv     # every memory scope and class the script guessed
  inflight/<card-id>.json, inflight/<card-id>.brief.md
  tasks/<card-id>/...    # copied task files (see the rules below)
  tasks-skipped.tsv      # what was not copied, and why
  history/note-archive.md
  config.json
  commitments.jsonl
  open-questions.md
  secret-scan.md
```

`tiller import` reads `manifest.json`, `projects.json`, `cards.jsonl`, `memory.jsonl` and `inflight/`, checks every file against the manifest, and checks the manifest counts against what it plans. Fields it does not read (such as `source`) are provenance for people.

### projects.json

One object per row in `data/projects.md`. Projects come only from the registry: a card naming an unregistered project does not create one.

```json
{"id":"alpha","repo":"owner/name","remote":"https://github.com/owner/name.git","fm_checkout":"projects/alpha",
 "default_branch":"main","delivery":"no-mistakes","capacity":4,"owner_host":"<host label>",
 "added":"2026-10-25","description":"<verbatim>","release_flow":null,
 "source":{"file":"data/projects.md","line":5}}
```

- **`repo`** is the GitHub `owner/name` in the registry line when it has one. Otherwise it comes from `git -C <home>/projects/<id> config remote.origin.url`, and the importer links PRs by it. Credentials in a remote are stripped. A checkout with no GitHub origin leaves `repo` null and adds an open question.
- `default_branch` comes from the checkout's `origin/HEAD`, else null.
- `capacity` comes from `config/project-capacity`. A value under 1 becomes null with an open question, because the importer refuses it.
- A project id must be a plain name (letters, digits, dot, dash, underscore, starting with a letter or digit, no `..`). A row with another id is skipped. A second row with the same id is dropped and asked about, and an `added` date that is not a calendar date becomes null.

### cards.jsonl

One line per record in `backlog.md` and `done-archive.md`. A repeated id is one card: `backlog.md` wins over `done-archive.md`, and the repeat is an open question.

```json
{"id":"alpha-fix","project":"alpha","title":"<verbatim title>","column":"queued","kind":"ship","priority":1,
 "since":"2026-10-30","hold":null,"pr":"https://github.com/owner/name/pull/3","report":"tasks/alpha-fix/report.md",
 "runs":[],"depends_on":[],"category":null,"size":null,"done":null,
 "body":["<the indented lines under the record, verbatim>"],"legacy_id":"alpha-fix",
 "source":{"file":"data/backlog.md","line":15}}
```

- **`body`** is the record's indented continuation lines, and **`legacy_id`** is the Firstmate id. The importer needs both.
- A card id must be a plain name, as for projects: an id like `../x` names files under `data/`, `state/` and the bundle, so the record is skipped and asked about.
- Every `(key: value)` group is read wherever it sits on the line; a group that starts like a field but does not parse stays in the title and is asked about. An empty title becomes the id.
- A `since`, done or `hold-until` value that is not a calendar date (or an RFC 3339 time, for `hold-until`) becomes null with an open question; a free-text `hold-until` is kept in the hold reason. The importer refuses such dates.
- **`kind`** is `ship`, `scout` or `captain`. Firstmate's `docs`, `ops`, `task` and any other kind become `other`.
- **`done.outcome`** is what the importer accepts: `merged`, `reported`, `answered`, `completed`, `abandoned`, `dropped`, `cancelled`, `failed`. Firstmate's `done` word means what the kind says: a ship is `merged`, a scout `reported`, a captain card `answered`, anything else `completed`. `completed` and `abandoned` pass through. An open box in the done archive with no outcome becomes `abandoned` and an open question.
- **`column`** is `done`, `working` (in the In flight section), `needs` (a captain hold) or `queued`.
- **`hold`** decodes `fm-hold-v1:<base64>` payloads verbatim; a payload without that prefix is kept as written. `blocked-by:` tokens make a `blocked` hold.
- **`depends_on`** keeps only ids that are cards in the bundle, because the importer refuses a dependency that is nowhere. A dropped one is an open question. When open cards block each other, the edge that closes the loop is dropped and asked about, because the importer refuses a cycle.
- **`pr`** is the first GitHub pull request URL on the record line.
- **`report`** points at a report file the bundle holds, else the task's `report.md` or `final-report.md`, else null.
- `runs` is empty: the script does not reconstruct attempts. `category` and `size` stay null.
- **`note-archive.md` is hold history, not memory and not cards.** It is copied to `history/note-archive.md`. An id that appears only there is not exported as open work, and is listed in `open-questions.md`.

### tasks/

For each card with a `data/<card-id>/` directory, the script copies **only top-level regular text files**:

- No symlinks, no directories (so no checkouts, worktrees or fake homes), no empty files.
- No file over 200,000 bytes, and no file whose first 8 KB holds a NUL byte (binary and media).
- No file with a secret-bearing name.

The same rules apply to the in-flight brief (`data/<id>/brief.md`, only when `data/<id>` is a real directory) and to the `state/<id>.meta` and `.status` files it reads. Anything refused there is left out and asked about. Everything skipped is listed in `tasks-skipped.tsv` with its reason. The task copy stops at half the Part G limit, and the script fails if the finished bundle passes **500 MB**.

### memory.jsonl

One entry per list item (`-`, `*`, `+`, `1.` or `1)`) in `captain.md`, `learnings.md` and `memory-archive.md`, plus each auto-memory file, with the text verbatim. A continuation keeps its blank lines and indent, minus the item's own two spaces. A line that is neither a list item nor a heading is prose, not an entry: the script lists where it is in `open-questions.md`.

```json
{"id":"mem-1","text":"<verbatim>","scope":"global","section":"<heading>","class":"rule","tier_marker":"a:2026-10-06",
 "secret_ref":false,"provenance":"legacy-import","rationale":null,"generalized":null,"reason":null,
 "source":{"file":"data/captain.md","line":4}}
```

The script only guesses; it never rewrites. Its guesses, each listed in `memory-guesses.tsv` and in `open-questions.md`:

- `class`: `captain.md` entries are `rule`; learnings and the archive are `note`. An entry that names Firstmate's own scripts or paths (`bin/fm-*`, `state/<id>`, `FM_HOME`) is `requirement`.
- `scope`: `project:<id>` when a registered project (longer than three characters) is named in the first 160 characters, else `global`.
- `secret_ref` is true when the text names a key, token, vault or `.env`.

The count check is in the output and the README: entries written against the list items and files found in the sources. They must agree.

### Other files

- `inflight/<card-id>.json` for each card in the `working` or `landing` column, from `state/<id>.meta` and `.status`. `recommendation` is always `drain`; the open question asks the first mate to change it to `adopt` or `respawn`. The brief is copied verbatim.
- `config.json` from `config/`: `crew-dispatch.json`, `lane-capacity`, `project-capacity`, `startup-memory-budget`, and the `captain-notify.sh` target. `quota_floor` is null.
- `commitments.jsonl`: dated holds, open AFK contracts, `state/when` entries, and a count of operational-inbox messages still on disk.

## The secret gate

After the bundle is written, the script scans every text file in it for the Part G patterns:

```
sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|github_pat_|AKIA[0-9A-Z]{16}|xox[baprs]-|-----BEGIN [A-Z ]*PRIVATE KEY|(api[_-]?key|token|password|secret)\s*[:=]\s*\S{8,}
```

The last alternative is case-insensitive, so `API_KEY=`, `GITHUB_TOKEN=` and `Password:` count.

- A hit nobody allowed stops the run with exit 3 and no `manifest.json`, so the bundle cannot be imported.
- `secret-scan.md` reports every hit as a bundle path, a line number and a pattern name. It never prints the matched text, and it never drops a hit silently.
- Only a path given with `--allow-secret-hit` is reported under "allowed" and does not fail the run; the script has no built-in list of paths to skip. A trailing `/` allows a whole directory.
- A file with a NUL byte cannot be read as text, so it is listed as a hit ("binary file, not scanned") instead of being skipped.
- Allow a hit only after reading it. Typical benign hits are prose that names token shapes, `?token=` in a local log URL, and `api_key=` in a code sample.

## Not written by the script

These need judgment, and `tiller import` does not read them. The first mate writes them after the run, from the same home:

- `capabilities.md`: every Firstmate capability the captain uses, with evidence of use and a `file:line` for how it works today. It is the input to the parity matrix.
- `regressions.md`: every incident the new system must survive, each with when, what happened, the required behavior, a check, and a source. End it with a **Spec sources** section naming the home's contract documents.
- `handoff-letter.md`: at most 1,500 words on how the captain works, each project's state, open threads and what to do first. Mark opinions as opinions.
- `CUTOVER.md`: freeze dispatch, drain or hand off each in-flight card, holds that need the captain, re-run the script with `--mode final`, stop watchers, repoint dotfiles, and how to fall back within two weeks.
- The "State at export" section of each `inflight/<id>.brief.md`, and the `adopt` or `respawn` calls.
- `baseline/` (optional), built from the session transcripts by one scout.
- The answers to `open-questions.md`.

Keep them beside the bundle, not inside it: the manifest hashes only what the script wrote, so a file added later would not match it. If you edit a file the importer reads, re-run the script instead.

## Check the script

`scripts/firstmate-export-check.sh [tiller-binary]` runs the script over the synthetic home in `scripts/fixtures/firstmate-home` and asserts each rule above: card `body` and `legacy_id`, the outcome and kind mappings, note-archive handling, the repo from a checkout's origin, task-dir copying (a symlink, a nested directory, an oversized file, a binary and a secret-named file are made at run time), the secret gate, an unchanged home, records the importer would refuse (a dependency cycle, impossible dates, a capacity of 0, an empty title, a duplicate project), ids and links that would reach outside the home, a plain directory below a home that is itself a repository, a clean `tiller import --dry-run` and a real `tiller import`, each into a scratch database. CI runs it in the `nix` job.
