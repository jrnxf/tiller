#!/usr/bin/env bash
# Offline checks for the eval suite. No model calls, so CI runs them on every
# push; the evals themselves run only on demand (evals/README.md).
#
# - Every case names a real role, has a prompt and at least one grader of a
#   known type, and its fixture builds without error.
# - Case names are unique, and no prompt appears in both splits.
# - Every role has at least one case in each split, and its model alias is
#   pinned in pins.env.
# - evals/judge.lock matches the pinned judge model and the bytes of every
#   llm rubric. --relock rewrites it after a deliberate rubric or judge change.
#
# Usage: evals/check.sh [--relock]
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
relock=0
case "${1:-}" in
"") ;;
--relock) relock=1 ;;
*) echo "usage: evals/check.sh [--relock]" >&2; exit 2 ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
	sha() { sha256sum | awk '{print $1}'; }
else
	sha() { shasum -a 256 | awk '{print $1}'; }
fi

fail=0
err() { echo "evals/check.sh: $*" >&2; fail=1; }
front() { awk -v k="$2" '/^---$/ { n++; next } n == 1 && $0 ~ "^" k ":" { sub("^" k ":[ ]*", ""); print; exit }' "$1"; }
body() { awk 'n >= 2 { print } /^---$/ { n++ }' "$1"; }

roles=()
for f in "$root"/roles/*.md; do
	[ "$(basename "$f")" = README.md ] || roles+=("$(basename "$f" .md)")
done

seen_names=" "
covered=" "
prompts_tuning=""
prompts_holdout=""
lock_lines=()
for split in tuning holdout; do
	for dir in "$here/cases/$split"/*/; do
		[ -d "$dir" ] || continue
		dir=${dir%/}
		name=$(basename "$dir")
		rel="cases/$split/$name"
		case "$seen_names" in *" $name "*) err "$rel: case name used twice" ;; esac
		seen_names="$seen_names$name "
		[ -f "$dir/prompt.md" ] || { err "$rel: no prompt.md"; continue; }
		role=$(front "$dir/prompt.md" role)
		[ -f "$root/roles/$role.md" ] && [ "$role" != README ] || err "$rel: role '$role' is not in roles/"
		for key in model append_system_prompt; do
			[ -z "$(front "$dir/prompt.md" "$key")" ] || err "$rel: $key is set by run.sh from the role, not by the case"
		done
		[ -n "$(body "$dir/prompt.md" | tr -d '[:space:]')" ] || err "$rel: empty prompt"
		# A value that opens with a quote must close with it, or the YAML
		# fails to parse and plugin eval drops the case.
		awk '/^---$/ { n++; next } n == 1 && /^[a-z_]+: *"/ && !/"$/ { bad = 1 } END { exit bad }' "$dir/prompt.md" ||
			err "$rel: a frontmatter value opens with a quote it does not close"
		covered="$covered$split:$role "
		digest=$(body "$dir/prompt.md" | sha)
		if [ "$split" = tuning ]; then prompts_tuning="$prompts_tuning$digest
"; else prompts_holdout="$prompts_holdout$digest
"; fi

		# The fixture builds without error, so a broken holdout case shows
		# up here and not first inside a sealed run.
		ws=$(mktemp -d)
		if ! "$here/scaffold.sh" "$dir" >"$ws.sh" 2>"$ws.log" ||
			! (cd "$ws" && HOME="$ws" GIT_CONFIG_NOSYSTEM=1 bash "$ws.sh") >"$ws.log" 2>&1; then
			err "$rel: fixture failed: $(tail -n 3 "$ws.log")"
		fi
		rm -rf "$ws" "$ws.sh" "$ws.log"

		graders=("$dir"/graders/*.md)
		[ -e "${graders[0]}" ] || { err "$rel: no graders"; continue; }
		for g in "${graders[@]}"; do
			type=$(front "$g" type)
			case "$type" in
			regex | tool_used | tool_order | file_exists) ;;
			llm) lock_lines+=("$(sha <"$g")  $rel/graders/$(basename "$g")") ;;
			*) err "$rel/graders/$(basename "$g"): unknown grader type '$type'" ;;
			esac
		done
	done
done

dups=$(comm -12 <(printf '%s' "$prompts_tuning" | sort) <(printf '%s' "$prompts_holdout" | sort))
[ -z "$dups" ] || err "a prompt appears in both the tuning and the holdout split"

for role in "${roles[@]}"; do
	for split in tuning holdout; do
		case "$covered" in *" $split:$role "*) ;; *) err "role $role has no $split case" ;; esac
	done
done

# shellcheck source=SCRIPTDIR/pins.env
. "$here/pins.env"
for role in "${roles[@]}"; do
	alias=$(front "$root/roles/$role.md" model)
	var="MODEL_$alias"
	[ -n "${!var:-}" ] || err "roles/$role.md: model alias '$alias' has no MODEL_$alias in pins.env"
done

lock="$here/judge.lock"
want=$({
	echo "# Pinned judge and rubrics. Regenerate with evals/check.sh --relock after a"
	echo "# deliberate change; a rubric edit re-baselines both splits."
	echo "judge $JUDGE_MODEL"
	printf '%s\n' "${lock_lines[@]}" | sort -k2
})
if [ "$relock" = 1 ]; then
	printf '%s\n' "$want" >"$lock"
	echo "evals/check.sh: wrote $(basename "$lock")"
elif [ "$want" != "$(cat "$lock" 2>/dev/null)" ]; then
	err "judge.lock is stale: a rubric or the judge model changed. Review the change, then run evals/check.sh --relock"
	diff <(cat "$lock" 2>/dev/null) <(printf '%s\n' "$want") >&2 || true
fi

exit "$fail"
