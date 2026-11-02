#!/usr/bin/env bash
# Start `tiller daemon` on a free port and check that it serves the embedded board.
# Usage: scripts/smoke.sh <path-to-tiller-binary>
set -euo pipefail

bin=${1:?usage: smoke.sh <path-to-tiller-binary>}
log=$(mktemp)
"$bin" daemon --port 0 >"$log" &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; rm -f "$log"' EXIT

url=
for _ in $(seq 1 50); do
  url=$(sed -n 's/^listening on //p' "$log")
  [ -n "$url" ] && break
  kill -0 "$pid" 2>/dev/null || break
  sleep 0.1
done
[ -n "$url" ] || { echo "daemon did not report an address:" >&2; cat "$log" >&2; exit 1; }

page=$(curl -fsS "$url/")
grep -q '<div id="root">' <<<"$page" || { echo "board shell missing from $url/" >&2; exit 1; }

asset=$(grep -o '/assets/[^"]*\.js' <<<"$page" | head -n 1)
[ -n "$asset" ] || { echo "no script asset referenced by the board shell" >&2; exit 1; }
headers=$(curl -fsS -D - -o /dev/null "$url$asset")
grep -qi '^cache-control: .*immutable' <<<"$headers" || { echo "$asset is not served immutable" >&2; exit 1; }

health=$(curl -fsS "$url/api/v1/health")
grep -q '"version"' <<<"$health" || { echo "no health answer from $url/api/v1/health" >&2; exit 1; }

echo "ok: $url serves the board and its health"
