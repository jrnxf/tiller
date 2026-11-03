#!/usr/bin/env bash
# Run the role behavior evals with `claude plugin eval --no-publish`.
#
# Each case under evals/cases/<split>/<case>/ names the role it tests with a
# `role:` line in its prompt.md frontmatter. This script assembles a
# throwaway plugin whose eval suite is those cases with the role's body
# appended to the system prompt (the way `tiller spawn` launches it), the
# role's model resolved through pins.env, and a scaffold that installs the
# stub `tiller` CLI plus the case's fixture.sh. Then it runs the suite with
# the pinned judge.
#
# The holdout split is sealed: it always runs whole, at each case's own run
# count and model, and this script prints only its aggregate. Its full
# results stay under <out>/sealed/. See evals/README.md.
#
# Usage: evals/run.sh [--split tuning|holdout] [--case GLOB] [--runs N]
#                     [--model M] [--out DIR] [--threshold T]
#                     [--max-cost-usd USD] [-j N] [--keep]
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)

split=tuning
case_glob=""
runs=""
model=""
out=""
threshold=0.8
max_cost=""
concurrency=1
keep=0

usage() { sed -n '/^# Usage:/,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; }
need() { [ "$2" -ge 2 ] || { echo "evals/run.sh: $1 needs a value" >&2; exit 2; }; }

while [ $# -gt 0 ]; do
	case "$1" in
	--split) need "$1" $#; split=$2; shift 2 ;;
	--case) need "$1" $#; case_glob=$2; shift 2 ;;
	--runs) need "$1" $#; runs=$2; shift 2 ;;
	--model) need "$1" $#; model=$2; shift 2 ;;
	--out) need "$1" $#; out=$2; shift 2 ;;
	--threshold) need "$1" $#; threshold=$2; shift 2 ;;
	--max-cost-usd) need "$1" $#; max_cost=$2; shift 2 ;;
	-j | --concurrency) need "$1" $#; concurrency=$2; shift 2 ;;
	--keep) keep=1; shift ;;
	-h | --help) usage; exit 0 ;;
	*) echo "evals/run.sh: unknown option: $1" >&2; usage >&2; exit 2 ;;
	esac
done

case "$split" in
tuning) ;;
holdout)
	# Narrowing the holdout to one case, fewer runs or another model is how
	# it turns into a tuning set, and kept transcripts or a moved pass mark
	# expose per-case results, so those knobs are refused here.
	if [ -n "$case_glob" ] || [ -n "$runs" ] || [ -n "$model" ] || [ "$keep" = 1 ] || [ "$threshold" != 0.8 ]; then
		echo "evals/run.sh: the holdout runs whole: --case, --runs, --model, --keep and --threshold are tuning-only" >&2
		exit 2
	fi
	;;
*) echo "evals/run.sh: --split must be tuning or holdout" >&2; exit 2 ;;
esac

# The offline checks guard what a score depends on: a stale judge lock or a
# malformed case refuses the run before any model call.
"$here/check.sh"

# shellcheck source=SCRIPTDIR/pins.env
. "$here/pins.env"

have_version=$(claude --version 2>/dev/null | awk '{print $1}')
if [ "$have_version" != "$CLAUDE_CODE_VERSION" ]; then
	echo "evals/run.sh: claude is ${have_version:-missing}, the pinned version is $CLAUDE_CODE_VERSION; scores are not comparable" >&2
	# CI and the holdout record scores, so they need the pinned version.
	if [ -n "${CI:-}" ] || [ "$split" = holdout ]; then exit 1; fi
fi

# The work dir also becomes the eval's TMPDIR, so each run's workspace lives
# under it. Two sandbox limits place it: the Bash sandbox keeps /tmp
# read-only, so a workspace there cannot be written; and it puts a Unix
# socket in each run's temp dir, whose path must stay under about 100 bytes,
# so a long inherited TMPDIR breaks every Bash call.
cache="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$cache"
work=$(mktemp -d "$cache/tiller-evals.XXXXXX")
if [ "$keep" = 1 ]; then
	echo "evals/run.sh: keeping $work" >&2
else
	trap 'rm -rf "$work"' EXIT
fi
[ -n "$out" ] || out=$(mktemp -d "${TMPDIR:-/tmp}/tiller-evals-out.XXXXXX")
mkdir -p "$out"
out=$(cd "$out" && pwd)

plugin="$work/plugin"
mkdir -p "$plugin/.claude-plugin" "$plugin/evals"
printf '{"name":"tiller-evals","version":"0.0.0","description":"Tiller role behavior evals"}\n' \
	>"$plugin/.claude-plugin/plugin.json"

# role_body ROLE - the prompt body of roles/ROLE.md, without its frontmatter.
role_body() { awk 'n >= 2 { print } /^---$/ { n++ }' "$root/roles/$1.md"; }
# role_model ROLE - the pinned model ID for the role's frontmatter alias.
role_model() {
	local alias var
	alias=$(awk '/^---$/ { n++; next } n == 1 && /^model:/ { print $2 }' "$root/roles/$1.md")
	var="MODEL_$alias"
	[ -n "${!var:-}" ] || { echo "evals/run.sh: no pin for model alias '$alias' (roles/$1.md)" >&2; exit 1; }
	printf '%s\n' "${!var}"
}

for src in "$here/cases/$split"/*/; do
	src=${src%/}
	name=$(basename "$src")
	dst="$plugin/evals/$name"
	mkdir -p "$dst"
	cp -R "$src/graders" "$dst/graders"
	role=$(awk '/^---$/ { n++; next } n == 1 && /^role:/ { print $2 }' "$src/prompt.md")

	# The role line moves out of prompt.md; the pinned model and the role
	# body go in case.yaml, whose YAML block scalar keeps the body's own
	# "---" table rules from being read as a frontmatter fence.
	awk '/^---$/ { n++ } !(n == 1 && /^role:/) { print }' "$src/prompt.md" >"$dst/prompt.md"

	"$here/scaffold.sh" "$src" >"$dst/scaffold.sh"
	# On its own line so an unpinned alias stops the script under set -e.
	case_model=$(role_model "$role")
	{
		printf 'schema_version: "1.1"\nname: %s\ntags: [%s, %s]\n' "$name" "$split" "$role"
		printf 'context:\n  scaffold_script: scaffold.sh\n'
		printf 'execution:\n  model: %s\n  append_system_prompt: |\n' "$case_model"
		role_body "$role" | sed 's/^/    /'
	} >"$dst/case.yaml"
done

args=(plugin eval "$plugin"
	--no-publish --trust-plugin --scaffold --ablation none
	--judge-model "$JUDGE_MODEL"
	--threshold "$threshold" --concurrency "$concurrency")
[ -z "$case_glob" ] || args+=(--case "$case_glob")
[ -z "$runs" ] || args+=(--runs "$runs")
[ -z "$model" ] || args+=(--model "$model")
[ -z "$max_cost" ] || args+=(--max-cost-usd "$max_cost")
[ "$keep" = 0 ] || args+=(--keep-temp)
# Bash, Edit and Write are what the roles work with; every Bash command runs
# in Claude Code's sandbox, confined to the run's workspace.
args+=(--allow-tools Bash Edit Write)

# The stub tiller is found through a relative PATH entry, which the Bash
# tool resolves in each run's own workspace.
export PATH=".tiller/bin:$PATH"
# A run's HOME is a fresh temp dir, which hides links into the user's
# Nix profile; the tools the fixtures need go on PATH by their real
# location.
for tool in node git; do
	bin=$(command -v "$tool") || { echo "evals/run.sh: $tool is not installed" >&2; exit 1; }
	PATH="$(dirname "$(readlink -f "$bin")"):$PATH"
done
# The sandbox can only start a shell from a system path, not a login shell
# from a home-manager profile, so every run uses bash.
SHELL=$(command -v bash)
export SHELL
export TMPDIR="$work"

rc=0
if [ "$split" = holdout ]; then
	mkdir -p "$out/sealed"
	claude "${args[@]}" --output-dir "$out/sealed" --json "$out/sealed/result.json" >/dev/null 2>"$out/sealed/stderr.log" || rc=$?
	node -e '
		const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
		const a = r.aggregates || {};
		const lines = [
			"holdout: " + a.casesPassed + " of " + a.casesTotal + " cases at or above the threshold",
			"overall score: " + (typeof a.overallScore === "number" ? a.overallScore.toFixed(2) : "n/a"),
			"cost: $" + (r.costUsd ?? 0).toFixed(2) + (r.partial ? " (partial: " + r.partialReason + ")" : ""),
		];
		console.log(lines.join("\n"));
	' "$out/sealed/result.json" | tee "$out/summary.txt" || { echo "evals/run.sh: no holdout result; exit $rc" >&2; }
	echo "sealed results: $out/sealed (do not read them while tuning; evals/README.md)"
else
	claude "${args[@]}" --output-dir "$out/results" --json "$out/result.json" >/dev/null || rc=$?
	node -e '
		const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
		for (const c of r.cases || []) {
			const s = c.aggregates && typeof c.aggregates.score === "number" ? c.aggregates.score.toFixed(2) : "n/a";
			console.log(s.padEnd(6) + c.name);
		}
		const a = r.aggregates || {};
		console.log("tuning: " + a.casesPassed + " of " + a.casesTotal + " cases passed; cost $" + (r.costUsd ?? 0).toFixed(2) + (r.partial ? " (partial: " + r.partialReason + ")" : ""));
	' "$out/result.json" | tee "$out/summary.txt" || echo "evals/run.sh: no result; exit $rc" >&2
	echo "results: $out/results"
fi

# plugin eval skips a case file it cannot load and still scores the rest, so
# a whole-split run checks that every case it was given was scored.
if [ -z "$case_glob" ]; then
	json="$out/result.json"
	[ "$split" = tuning ] || json="$out/sealed/result.json"
	want=$(find "$plugin/evals" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')
	got=$(node -e 'try { console.log(require(process.argv[1]).aggregates.casesTotal) } catch { console.log(0) }' "$json")
	# A run stopped by --max-cost-usd is partial and says so in its summary.
	partial=$(node -e 'try { console.log(require(process.argv[1]).partial ? 1 : 0) } catch { console.log(0) }' "$json")
	if [ "$got" != "$want" ] && [ "$partial" = 0 ]; then
		echo "evals/run.sh: $got of $want cases were scored; a case failed to load" >&2
		[ "$rc" != 0 ] || rc=1
	fi
fi
exit "$rc"
