#!/usr/bin/env bash
# The usability bench's stand-in for `gh` (scripts/usability-bench.sh writes a
# wrapper that sets the variables below and runs this file as TILLER_GH). It
# answers the `gh api` calls the daemon makes for a pull request: open, read,
# edit, checks, reviews, merge, and the reads after a merge. It talks to no
# network. A repository `example/<id>` is the bare repository
# $BENCH_ORIGINS/<id>.git, and the pull requests are JSON files under
# $BENCH_FORGE/<id>/.
#
#   BENCH_FORGE    the folder of the pull request files
#   BENCH_ORIGINS  the folder of the bare repositories
#   BENCH_JQ       jq
#   BENCH_GIT      git
#
# Files of one repository, which `usability-bench.sh forge` also writes:
#   <n>.json           the pull request, as GitHub's REST API prints it
#   <n>.checks         success, failure or pending (success when absent)
#   <n>.reviews.json   the list of reviews ([] when absent)
set -euo pipefail

jq=${BENCH_JQ:?}
git=${BENCH_GIT:?}
forge=${BENCH_FORGE:?}
origins=${BENCH_ORIGINS:?}

if [ "${1:-}" = --version ]; then
  echo "gh version 0.0.0 (usability bench stand-in)"
  exit 0
fi
[ "${1:-}" = api ] || { echo "the gh stand-in answers only \`gh api\`: $*" >&2; exit 1; }
shift

method=GET
path=
filter=
slurp=0
fields=()
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method=$2; shift 2 ;;
    -f) fields+=("$2"); shift 2 ;;
    --jq) filter=$2; shift 2 ;;
    --slurp) slurp=1; shift ;;
    --paginate) shift ;;
    *) path=$1; shift ;;
  esac
done

query=
case "$path" in *\?*) query=${path#*\?}; path=${path%%\?*} ;; esac

fail() {
  echo "gh: $1 (HTTP $2)" >&2
  exit 1
}

# Prints one answer the way `gh api` would with the caller's flags.
answer() {
  local body
  body=$(cat)
  if [ "$slurp" = 1 ]; then body="[$body]"; fi
  if [ -n "$filter" ]; then
    printf '%s' "$body" | "$jq" -r "$filter"
  else
    printf '%s\n' "$body"
  fi
  exit 0
}

# The value of one `-f key=value` field.
field() {
  local entry
  for entry in ${fields[@]+"${fields[@]}"}; do
    case "$entry" in "$1="*) printf '%s' "${entry#*=}"; return 0 ;; esac
  done
}

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }

case "$path" in
  repos/*/*) ;;
  *) fail "Not Found" 404 ;;
esac
rest=${path#repos/}
owner=${rest%%/*}
rest=${rest#*/}
name=${rest%%/*}
rest=${rest#"$name"}
rest=${rest#/}
origin=$origins/$name.git
dir=$forge/$name
[ -d "$origin" ] || fail "Not Found" 404
mkdir -p "$dir"

g() { "$git" -C "$origin" "$@"; }

# pull N: the pull request as it is now. An open one's head follows its branch,
# as a push to GitHub would move it.
pull() {
  local file=$dir/$1.json sha
  [ -f "$file" ] || fail "Not Found" 404
  if [ "$("$jq" -r .state "$file")" = open ]; then
    sha=$(g rev-parse -q --verify "refs/heads/$("$jq" -r .head.ref "$file")" || true)
    if [ -n "$sha" ]; then
      "$jq" --arg sha "$sha" '.head.sha = $sha' "$file"
      return 0
    fi
  fi
  cat "$file"
}

checks_of() {
  local file state
  for file in "$dir"/*.json; do
    [ -f "$file" ] || continue
    case "$file" in *.reviews.json) continue ;; esac
    if [ "$(pull "$(basename "$file" .json)" | "$jq" -r .head.sha)" = "$1" ]; then
      state=$(cat "${file%.json}.checks" 2>/dev/null || echo success)
      echo "$state"
      return 0
    fi
  done
  echo success
}

case "$method $rest" in
  "GET pulls")
    # The open pull requests of one head branch: `head=<owner>:<branch>`.
    branch=$(printf '%s' "$query" | tr '&' '\n' | sed -n 's/^head=[^:]*://p')
    {
      for file in "$dir"/*.json; do
        [ -f "$file" ] || continue
        case "$file" in *.reviews.json) continue ;; esac
        pull "$(basename "$file" .json)"
      done
    } | "$jq" -s --arg branch "$branch" \
      '[.[] | select(.state == "open" and ($branch == "" or .head.ref == $branch))]' | answer
    ;;
  "POST pulls")
    head=$(field head)
    base=$(field base)
    sha=$(g rev-parse -q --verify "refs/heads/$head") || fail "Validation Failed: no branch $head" 422
    number=1
    while [ -f "$dir/$number.json" ]; do number=$((number + 1)); done
    "$jq" -n --argjson number "$number" --arg title "$(field title)" --arg body "$(field body)" \
      --arg head "$head" --arg base "$base" --arg sha "$sha" --arg at "$(now)" \
      --arg url "https://example.com/$owner/$name/pull/$number" \
      '{number: $number, html_url: $url, state: "open", merged: false, title: $title,
        body: $body, created_at: $at, mergeable_state: "clean", merged_at: null,
        merge_commit_sha: null, merged_by: null, user: {login: "owner"},
        head: {ref: $head, sha: $sha}, base: {ref: $base, repo: {private: true}}}' \
      >"$dir/$number.json"
    pull "$number" | answer
    ;;
  "GET pulls/"*/files)
    number=${rest#pulls/}
    number=${number%/files}
    view=$(pull "$number")
    base=$(printf '%s' "$view" | "$jq" -r .base.ref)
    sha=$(printf '%s' "$view" | "$jq" -r .head.sha)
    g diff --name-only "refs/heads/$base...$sha" | "$jq" -R -s 'split("\n") | map(select(. != "") | {filename: .})' | answer
    ;;
  "GET pulls/"*/reviews)
    number=${rest#pulls/}
    number=${number%/reviews}
    if [ -f "$dir/$number.reviews.json" ]; then cat "$dir/$number.reviews.json"; else echo '[]'; fi | answer
    ;;
  "PUT pulls/"*/merge)
    number=${rest#pulls/}
    number=${number%/merge}
    view=$(pull "$number")
    [ "$(printf '%s' "$view" | "$jq" -r .state)" = open ] || fail "Pull Request is not mergeable" 405
    sha=$(printf '%s' "$view" | "$jq" -r .head.sha)
    base=$(printf '%s' "$view" | "$jq" -r .base.ref)
    [ "$(field sha)" = "$sha" ] || fail "Head branch was modified. Review and try the merge again." 409
    [ "$(checks_of "$sha")" != failure ] || fail "Required status check is failing" 405
    # The stand-in merges by fast-forward only: the base must not have moved.
    g merge-base --is-ancestor "refs/heads/$base" "$sha" || fail "Pull Request is not mergeable" 405
    g update-ref "refs/heads/$base" "$sha"
    printf '%s' "$view" | "$jq" --arg at "$(now)" --arg sha "$sha" \
      '.state = "closed" | .merged = true | .merged_at = $at | .merge_commit_sha = $sha
       | .merged_by = {login: "owner"}' >"$dir/$number.json.new"
    mv "$dir/$number.json.new" "$dir/$number.json"
    "$jq" -n --arg sha "$sha" '{merged: true, sha: $sha, message: "Pull Request successfully merged"}' | answer
    ;;
  "GET pulls/"*)
    pull "${rest#pulls/}" | answer
    ;;
  "PATCH pulls/"*)
    number=${rest#pulls/}
    pull "$number" | "$jq" --arg title "$(field title)" --arg body "$(field body)" \
      '.title = $title | .body = $body' >"$dir/$number.json.new"
    mv "$dir/$number.json.new" "$dir/$number.json"
    pull "$number" | answer
    ;;
  "GET commits/"*/check-runs)
    sha=${rest#commits/}
    sha=${sha%/check-runs}
    case "$(checks_of "$sha")" in
      failure) run='{"name":"ci","status":"completed","conclusion":"failure"}' ;;
      pending) run='{"name":"ci","status":"in_progress","conclusion":null}' ;;
      *) run='{"name":"ci","status":"completed","conclusion":"success"}' ;;
    esac
    echo "{\"total_count\":1,\"check_runs\":[$run]}" | answer
    ;;
  "GET commits/"*/status)
    echo '{"state":"success","statuses":[]}' | answer
    ;;
  "GET commits/"*/pulls)
    echo '[]' | answer
    ;;
  "GET commits/"*)
    sha=$(g rev-parse -q --verify "${rest#commits/}^{commit}") || fail "No commit found" 422
    "$jq" -n --arg sha "$sha" '{sha: $sha}' | answer
    ;;
  "GET compare/"*)
    range=${rest#compare/}
    from=${range%%...*}
    to=${range#*...}
    a=$(g rev-parse -q --verify "$from^{commit}") || fail "Not Found" 404
    b=$(g rev-parse -q --verify "$to^{commit}") || fail "Not Found" 404
    if [ "$a" = "$b" ]; then status=identical
    elif g merge-base --is-ancestor "$a" "$b"; then status=ahead
    elif g merge-base --is-ancestor "$b" "$a"; then status=behind
    else status=diverged; fi
    "$jq" -n --arg status "$status" '{status: $status}' | answer
    ;;
  "GET rules/branches/"* | "GET deployments" | "GET deployments/"*)
    echo '[]' | answer
    ;;
  "GET actions/workflows/"*)
    echo '{"total_count":0,"workflow_runs":[]}' | answer
    ;;
  *)
    fail "Not Found" 404
    ;;
esac
