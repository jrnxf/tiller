#!/usr/bin/env bash
# Run scripts/firstmate-export over the synthetic home in scripts/fixtures/firstmate-home
# and check the bundle against what `tiller import` reads.
# Usage: scripts/firstmate-export-check.sh [path-to-tiller-binary]   (default: target/debug/tiller)
set -euo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
bin=${1:-$root/target/debug/tiller}
export_script=$root/scripts/firstmate-export
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() { echo "firstmate-export-check: $*" >&2; exit 1; }

hash_tree() {
  # The whole tree, modes and link targets included, as one digest.
  (cd "$1" && find . -print0 | LC_ALL=C sort -z | xargs -0 ls -ld | sed 's/  */ /g' \
    && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 cat) | { sha256sum 2>/dev/null || shasum -a 256; }
}

home=$work/home
cp -R "$root/scripts/fixtures/firstmate-home" "$home"

# What a fixture cannot carry: real git checkouts, a symlink, a nested dir, an oversized
# file, a secret-named file and a secret-looking line.
for p in widget gizmo notes review; do
  mkdir -p "$home/projects/$p"
  git -C "$home/projects/$p" init -q
done
at=@
git -C "$home/projects/gizmo" remote add origin "https://user:pw${at}github.com/example-org/gizmo.git"
git -C "$home/projects/notes" remote add origin "ssh://git${at}git.example.com/example-org/notes.git"
git -C "$home/projects/review" remote add origin "ssh://git${at}review.example.com:29418/example-org/review"
task=$home/data/widget-fix-login
ln -s brief.md "$task/link.md"
mkdir -p "$task/checkout/src"
echo 'nested' > "$task/checkout/src/file.txt"
head -c 300000 /dev/zero | tr '\0' 'a' > "$task/big.log"
printf 'placeholder\n' > "$task/.env"
printf '\000binary' > "$task/blob.dat"
printf 'api_key = %s\n' "$(printf 'abcd1234')efgh5678" >> "$home/data/gizmo-report/report.md"

# Records the importer would refuse if the bundle carried them as they are: a dependency
# cycle, impossible dates, free-text hold-until, an empty title, a raw hold, a field in
# the middle of a title. Plus ids that would climb out of the home, and task and state
# files that are links to files outside it.
cat >> "$home/data/backlog.md" <<'MD'

## Queued

- [ ] widget-cycle-a - Cycle part A blocked-by: widget-cycle-b (repo: widget)
- [ ] widget-cycle-b - Cycle part B blocked-by: widget-cycle-a (repo: widget)
- [ ] widget-baddate - Impossible dates (repo: widget) (since 2026-13-45) (hold-until: next week)
- [ ] widget-empty - (repo: widget)
- [ ] widget-peel - Fields (since 2026-10-30 14:02) in the middle (repo: widget) of the title (kind: ship)
- [ ] widget-rawhold - Raw hold text (repo: widget) (hold: Waiting for the owner)

## In flight

- [ ] widget-linked - A task dir that is a link (repo: widget) (kind: ship)
- [ ] ../../evil - An id that climbs out (repo: widget) (kind: ship)
MD
outside=$work/outside
mkdir -p "$outside/taskdir"
echo 'OUTSIDE-MARKER brief' > "$outside/taskdir/brief.md"
echo 'working: OUTSIDE-MARKER status' > "$outside/status"
ln -s "$outside/taskdir" "$home/data/widget-linked"
ln -s "$outside/status" "$home/state/widget-linked.status"

cat >> "$home/data/projects.md" <<'MD'
- plain [local-only] - A plain directory that is not a checkout (added 2026-04-01)
- review [no-mistakes] - A checkout whose origin is Gerrit (added 2026-04-02)
- widget [local-only] - A second row for widget
- zero [local-only] - A project whose capacity is zero (added 2026-02-30)
- ../etc [local-only] - An id that climbs out
MD
printf 'zero 0\n' >> "$home/config/project-capacity"
mkdir -p "$home/projects/plain"
# The home is itself a repository with a remote: a plain project directory below it must
# not report that remote.
git -C "$home" init -q
git -C "$home" remote add origin "https://github.com/example-org/home-repo.git"

cat >> "$home/data/captain.md" <<'MD'
* Star bullet entry.
1. Numbered entry.
Loose prose that is not a list item.
- Spaced entry first line.

  Second paragraph.
    Indented detail.
MD

before=$(hash_tree "$home")
run() { node "$export_script" --home "$home" --auto-memory none --host-label example-host --project-alias oldblip=widget \
  --repo-alias example-org/gizmo-old=example-org/gizmo "$@"; }

# A secret hit nobody allowed stops the run: exit 3, reported by path, no manifest.
set +e
run --out "$work/refused" >"$work/refused.out" 2>"$work/refused.err"
status=$?
set -e
[ "$status" -eq 3 ] || fail "an unallowed secret hit must exit 3, got $status"
[ ! -e "$work/refused/manifest.json" ] || fail "a refused export must not write manifest.json"
grep -q 'tasks/gizmo-report/report.md:' "$work/refused/secret-scan.md" || fail "secret-scan.md must name the hit"
grep -q 'abcd1234' "$work/refused/secret-scan.md" && fail "secret-scan.md must not print the matched text"

# The same hit, allowed by path, is reported and the run succeeds.
bundle=$work/bundle
run --out "$bundle" --allow-secret-hit tasks/gizmo-report/report.md >"$work/run.out"
grep -q 'tasks/gizmo-report/report.md:' "$bundle/secret-scan.md" || fail "an allowed hit must still be listed"

[ "$(hash_tree "$home")" = "$before" ] || fail "the export changed the Firstmate home"

BUNDLE=$bundle node - <<'EOF'
const fs = require('node:fs')
const path = require('node:path')
const crypto = require('node:crypto')
const dir = process.env.BUNDLE
const read = (f) => fs.readFileSync(path.join(dir, f), 'utf8')
const jsonl = (f) => read(f).split('\n').filter(Boolean).map((l) => JSON.parse(l))
const check = (ok, message) => {
  if (!ok) {
    console.error(`firstmate-export-check: ${message}`)
    process.exit(1)
  }
}
const manifest = JSON.parse(read('manifest.json'))
const cards = jsonl('cards.jsonl')
const projects = JSON.parse(read('projects.json'))
const memory = jsonl('memory.jsonl')
const card = (id) => cards.find((c) => c.id === id)

check(manifest.schema === 'tiller-import/v1', 'schema')
check(manifest.counts.cards === cards.length && manifest.counts.projects === projects.length, 'manifest counts')
check(manifest.counts.memory === memory.length, 'manifest memory count')
for (const [rel, sha] of Object.entries(manifest.files)) {
  check(crypto.createHash('sha256').update(fs.readFileSync(path.join(dir, rel))).digest('hex') === sha, `${rel} sha256`)
}

// Cards carry body and legacy_id.
check(cards.every((c) => Array.isArray(c.body) && c.legacy_id === c.id), 'every card has body and legacy_id')
check(card('widget-fix-login').body.join('\n').includes('Users bounce'), 'a card body is kept')

// Done outcomes: done, completed, abandoned and the kind-owned meaning of done.
check(card('widget-old-task').done.outcome === 'merged', 'done on a ship card is merged')
check(card('gizmo-survey').done.outcome === 'reported', 'done on a scout card is reported')
check(card('gizmo-docs-pass').done.outcome === 'completed', 'completed stays completed')
check(card('widget-dropped').done.outcome === 'abandoned', 'abandoned stays abandoned')
check(cards.filter((c) => c.id === 'widget-old-task').length === 1, 'a repeated id is one card')
check(card('gizmo-renamed').pr === 'https://github.com/example-org/gizmo/pull/9', 'a renamed repo is mapped by --repo-alias')
check(card('widget-shipped').pr === 'https://github.com/example-org/widget/pull/41' && card('widget-shipped').project === 'widget', 'a PR url is read from the record')

// Kinds docs, ops and task map to other.
for (const id of ['widget-write-docs', 'widget-rotate', 'widget-vendor-pick', 'gizmo-docs-pass']) {
  check(card(id).kind === 'other', `${id} kind is other`)
}

// note-archive.md is hold history, not open work.
check(!card('widget-gone'), 'a note-archive-only id is not a card')
check(fs.existsSync(path.join(dir, 'history/note-archive.md')), 'note-archive.md is kept as history')
check(read('open-questions.md').includes('widget-gone'), 'the note-archive-only id is an open question')

// Repo from the registry, else from the checkout's origin, never with credentials.
const project = (id) => projects.find((p) => p.id === id)
check(project('widget').repo === 'example-org/widget', 'repo from the registry')
check(project('gizmo').repo === 'example-org/gizmo', 'repo from the checkout origin')
check(!JSON.stringify(project('gizmo')).includes('pw@'), 'no credentials in a remote')
check(project('notes').repo === null, 'a non-GitHub origin leaves repo null')
check(project('widget').capacity === 2 && project('gizmo').capacity === 1, 'capacity from config')
check(!projects.some((p) => p.id === 'oldblip'), 'no project is invented from a card')
check(card('oldblip-thing').project === 'widget', 'project alias applied')
check(card('notes-tidy').project === 'notes', 'owner/name repo resolves to the project')

// Holds and dependencies.
check(card('gizmo-report').hold.reason === 'Waiting for the owner to pick a vendor.', 'hold payload decoded')
check(card('widget-vendor-pick').depends_on.join() === 'widget-fix-login', 'only bundled dependencies are kept')
check(read('open-questions.md').includes('widget-ghost'), 'a dropped dependency is an open question')
check(card('notes-tidy').hold.kind === 'date' && card('notes-tidy').hold.until === '2026-11-20', 'date hold')
check(card('widget-fix-login').column === 'working', 'in flight is working')

// Task dirs: top-level regular text files only.
const taskFiles = fs.readdirSync(path.join(dir, 'tasks/widget-fix-login')).sort()
check(taskFiles.join() === 'brief.md,notes.txt', `task files were ${taskFiles.join()}`)
const skipped = read('tasks-skipped.tsv')
for (const name of ['link.md\tsymlink', 'checkout\tdirectory', 'big.log\tover', '.env\tsecret-bearing name', 'blob.dat\tnot text']) {
  check(skipped.includes(`widget-fix-login/${name}`), `tasks-skipped.tsv lists ${name}`)
}
check(card('gizmo-report').report === 'tasks/gizmo-report/report.md', 'report path points into the bundle')

// Memory is verbatim.
const spawn = memory.find((m) => m.text.startsWith('Run bin/fm-spawn.sh'))
check(spawn && spawn.class === 'requirement', 'a Firstmate script entry is a requirement')
check(memory.find((m) => m.text.includes('Continue the build after that')), 'a continuation line stays in its entry')
check(memory.find((m) => m.text.includes('vault')).secret_ref === true, 'a secret reference is flagged')
check(memory.find((m) => m.text.includes('<!--a:2026-10-06-->')).tier_marker === 'a:2026-10-06', 'tier marker extracted, text kept')

// In-flight card.
const inflight = JSON.parse(read('inflight/widget-fix-login.json'))
check(inflight.recommendation === 'drain' && inflight.last_status === 'working: reproduced the loop', 'inflight record')
check(manifest.counts.inflight === 2, 'inflight count')
const questions = read('open-questions.md')

// A record id that is not a plain name is skipped, and nothing is written outside the bundle.
check(!cards.some((c) => c.id.includes('..')), 'an id with .. is not a card')
check(questions.includes('is not a plain card id'), 'a bad card id is an open question')
check(!fs.existsSync(path.join(dir, '..', 'evil.json')) && !fs.existsSync(path.join(dir, 'inflight', '..', '..', 'evil.json')), 'no file escapes the bundle')
check(!projects.some((p) => p.id.includes('..')) && questions.includes('is not a plain project id'), 'a bad project id is skipped and asked')

// A checkout that is not itself a repository does not borrow the home's remote.
check(project('plain') && project('plain').repo === null, 'a plain directory has no repo')
check(JSON.stringify(projects).indexOf('home-repo') === -1, 'the home repository remote is not used')

// A linked task directory, brief or status file is not followed.
const linked = JSON.parse(read('inflight/widget-linked.json'))
check(linked.last_status === null, 'a linked status file is not read')
check(!read('inflight/widget-linked.brief.md').includes('OUTSIDE-MARKER'), 'a linked brief is not copied')
check(!fs.existsSync(path.join(dir, 'tasks/widget-linked')), 'a linked task directory is not copied')

// What the importer would refuse becomes an open question and a valid value.
check(card('widget-cycle-a').depends_on.length + card('widget-cycle-b').depends_on.length === 1, 'one edge of a cycle is dropped')
check(questions.includes('block each other'), 'a cycle is an open question')
check(project('zero').capacity === null && questions.includes('capacity 0'), 'capacity 0 becomes null with a question')
check(project('zero').added === null, 'an impossible project date becomes null')
check(project('review').delivery === 'local-only' && questions.includes('Gerrit remote'), 'a Gerrit origin makes delivery local-only with a question')
check(projects.filter((p) => p.id === 'widget').length === 1 && project('widget').capacity === 2, 'a duplicate project row is dropped')
check(card('widget-baddate').since === null && card('widget-baddate').hold.until === null, 'impossible and free-text dates become null')
check(card('widget-baddate').hold.reason === 'hold until next week', 'a free-text hold-until is kept in the reason')
check(card('widget-empty').title === 'widget-empty' && questions.includes('has no title'), 'an empty title falls back to the id')
check(card('widget-peel').title === 'Fields in the middle of the title' && card('widget-peel').kind === 'ship' && card('widget-peel').project === 'widget' && card('widget-peel').since === '2026-10-30', 'fields are read wherever they sit')
check(card('widget-rawhold').hold.reason === 'Waiting for the owner', 'a hold without the fm-hold-v1 prefix is kept as written')

// Memory: every list item is an entry, continuations keep their text, prose is asked about.
check(memory.find((m) => m.text === 'Star bullet entry.') && memory.find((m) => m.text === 'Numbered entry.'), 'star and numbered items are entries')
check(memory.find((m) => m.text === 'Spaced entry first line.\n\nSecond paragraph.\n  Indented detail.'), 'a continuation keeps its blank line and indent')
check(questions.includes('prose, not list items') && questions.includes('data/captain.md:'), 'prose lines in memory files are an open question')
const readme = read('README.md')
const counts = /memory entries: (\d+) \(sources hold (\d+)\)/.exec(readme)
check(counts && counts[1] === counts[2] && Number(counts[1]) === memory.length, 'every list item in the memory files is an entry')
EOF

# The importer accepts the bundle: no schema error, nothing written (--dry-run).
export TILLER_DB=$work/state.db
"$bin" init --host-label example-host --lane-capacity 2 >/dev/null
"$bin" --json import --dry-run "$bundle" >"$work/import.json" 2>"$work/import.err" \
  || { cat "$work/import.err" "$work/import.json" >&2; fail "tiller import --dry-run refused the bundle"; }
IMPORT_JSON=$work/import.json node -e '
  const plan = JSON.parse(require("node:fs").readFileSync(process.env.IMPORT_JSON, "utf8")).data
  const bad = (message) => { console.error("firstmate-export-check: " + message); process.exit(1) }
  if (plan.checks.length === 0 || plan.checks.some((c) => !c.ok || c.manifest !== c.planned)) bad("a count check failed: " + JSON.stringify(plan.checks))
  const linked = plan.cards.filter((c) => c.pr)
  if (linked.length !== 3) bad("expected 3 PR links, got " + linked.length)
  const unlinked = linked.filter((c) => c.pr.not_linked)
  if (unlinked.length > 0) bad("PR links not linked: " + JSON.stringify(unlinked.map((c) => c.pr)))
  if (plan.cards.find((c) => c.legacy_id === "gizmo-done-ship").pr.project_id !== "gizmo") bad("the gizmo PR must link through the checkout origin")
'

# The same bundle imported for real: every write-time check passes too.
"$bin" import "$bundle" >"$work/apply.out" 2>"$work/apply.err" \
  || { cat "$work/apply.err" "$work/apply.out" >&2; fail "tiller import refused the bundle when writing"; }

# A history file that is not text is left out, with a question, instead of copied unscanned.
home2=$work/home2
cp -R "$home" "$home2"
printf '\000binary' >> "$home2/data/note-archive.md"
node "$export_script" --home "$home2" --out "$work/bundle2" --auto-memory none --host-label example-host \
  --project-alias oldblip=widget --repo-alias example-org/gizmo-old=example-org/gizmo \
  --allow-secret-hit tasks/gizmo-report/report.md >/dev/null
[ ! -e "$work/bundle2/history/note-archive.md" ] || fail "a binary note-archive.md must not be copied"
grep -q 'note-archive.md was not copied' "$work/bundle2/open-questions.md" || fail "a skipped note-archive.md must be an open question"

echo "ok: the exporter bundle passes tiller import --dry-run and a real import"
