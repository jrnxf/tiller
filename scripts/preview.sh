#!/usr/bin/env bash
# The live preview: the usability bench (scripts/usability-bench.sh) on fixed
# ports, behind a reverse proxy, rebuilt from this checkout as it changes. A
# board edit shows on the next page load (vite build --watch writes
# board/dist, which a debug daemon reads from disk). A Rust edit rebuilds the
# binary and restarts both daemons on the same ports, so open tabs reconnect.
# There is one preview per machine: `up` from another worktree moves it there.
# Each `up` seeds fresh homes, but the owner's board sessions (`board-owner`,
# 30 days each) are kept in $TILLER_PREVIEW_STATE (default
# ~/.local/state/tiller-preview), so a browser signs in once, not after
# every `up`. A sign-in link itself lasts one minute.
#
# Usage, inside `nix develop`:
#   scripts/preview.sh up [--no-build]   build, seed, start, watch; print the sign-in link
#   scripts/preview.sh down              stop the watcher and the bench
#   scripts/preview.sh status            which checkout it follows and what runs
#   scripts/preview.sh login [main|peer] a fresh owner sign-in link and code (default main)
#   scripts/preview.sh log               follow the watcher's build log
#   scripts/preview.sh bench ARGS        any usability-bench.sh command on the preview
#
# The proxy's names are this machine's, so they live outside the repository,
# in $TILLER_PREVIEW_ENV (default ~/.config/tiller/preview.env), for example:
#   TILLER_BENCH_MAIN_HOSTS="tiller-preview.example.com"
#   TILLER_BENCH_PEER_HOSTS="peer.tiller-preview.example.com"
# The ports default to 7431 (main) and 7432 (peer); the file may set
# TILLER_BENCH_MAIN_PORT and TILLER_BENCH_PEER_PORT. With no names the links
# are loopback ones.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
env_file=${TILLER_PREVIEW_ENV:-$HOME/.config/tiller/preview.env}
if [ -f "$env_file" ]; then
  # shellcheck source=/dev/null
  . "$env_file"
fi
export TILLER_BENCH_DIR=${TILLER_PREVIEW_DIR:-/tmp/tiller-preview}
export TILLER_BENCH_MAIN_PORT=${TILLER_BENCH_MAIN_PORT:-7431}
export TILLER_BENCH_PEER_PORT=${TILLER_BENCH_PEER_PORT:-7432}
export TILLER_BENCH_MAIN_HOSTS=${TILLER_BENCH_MAIN_HOSTS:-}
export TILLER_BENCH_PEER_HOSTS=${TILLER_BENCH_PEER_HOSTS:-}
export TILLER_BENCH_STOP="scripts/preview.sh down"
dir=$TILLER_BENCH_DIR
bench=$repo/scripts/usability-bench.sh
log=$dir/watch.log
state=${TILLER_PREVIEW_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/tiller-preview}

# Copies each home's board sessions to $state, and back. A daemon reads its
# `board-owner` again within a second of a change, so a restore needs no
# restart.
save_sessions() {
  local host file
  for host in main peer; do
    file=$dir/$host/home/board-owner
    [ -s "$file" ] || continue
    cmp -s "$file" "$state/$host-board-owner" && continue
    mkdir -p "$state"
    chmod 700 "$state"
    cp "$file" "$state/$host-board-owner.tmp" && mv "$state/$host-board-owner.tmp" "$state/$host-board-owner"
  done
}

restore_sessions() {
  local host
  for host in main peer; do
    [ -s "$state/$host-board-owner" ] || continue
    install -m 600 "$state/$host-board-owner" "$dir/$host/home/board-owner"
  done
}

alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

# The watcher runs in a process group of its own, so a stop ends vite and a
# cargo build with it.
stop_watcher() {
  local pid
  alive "$dir/watch.pid" || { rm -f "$dir/watch.pid"; return 0; }
  pid=$(cat "$dir/watch.pid")
  kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 50); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  kill -9 -- "-$pid" 2>/dev/null || true
  rm -f "$dir/watch.pid"
}

start_watcher() {
  local group=()
  command -v setsid >/dev/null && group=(setsid)
  echo "$repo" >"$dir/checkout"
  "${group[@]}" nohup "$repo/scripts/preview.sh" watch >>"$log" 2>&1 </dev/null &
  echo $! >"$dir/watch.pid"
}

# The Rust inputs, one path per line.
rust_inputs() {
  find "$repo/crates" "$repo/Cargo.toml" "$repo/Cargo.lock" \
    \( -name target -prune \) -o -type f \( -name '*.rs' -o -name '*.toml' -o -name Cargo.lock \) "$@" -print
}

stamp() { TZ=Europe/Lisbon date '+%a %Y-%m-%d %H:%M:%S'; }

# The watcher's loop: vite rebuilds the board on its own; this polls the Rust
# inputs every 2 s and, after a change, rebuilds and restarts both daemons.
watch() {
  cd "$repo"
  echo "$(stamp) watching $repo"
  bun run --cwd board build --watch &
  # A change is a file newer than the last build's start, or a file count
  # that moved (a removed file is newer than nothing).
  local count now
  touch "$dir/rust.stamp"
  count=$(rust_inputs | wc -l)
  while sleep 2; do
    save_sessions
    now=$(rust_inputs | wc -l)
    [ "$now" = "$count" ] && [ -z "$(rust_inputs -newer "$dir/rust.stamp" | head -n 1)" ] && continue
    count=$now
    touch "$dir/rust.stamp"
    echo "$(stamp) Rust changed: building"
    if cargo build -p tiller-cli --features tiller-core/test-owner &&
      cargo build -p tiller-panes --features testing --bin fake-agent &&
      "$bench" daemon peer restart && "$bench" daemon main restart; then
      echo "$(stamp) daemons restarted on the new build"
    else
      echo "$(stamp) the build or the restart failed: see above"
    fi
  done
}

up() {
  save_sessions
  stop_watcher
  "$bench" up "$@"
  restore_sessions
  start_watcher
  echo "the preview follows $repo"
  echo "build log: scripts/preview.sh log"
}

down() {
  save_sessions
  stop_watcher
  "$bench" down
}

status() {
  if [ -f "$dir/checkout" ]; then echo "checkout: $(cat "$dir/checkout")"; fi
  if alive "$dir/watch.pid"; then echo "watcher: up"; else echo "watcher: DOWN"; fi
  "$bench" status
}

command=${1:-}
[ $# -gt 0 ] && shift
case "$command" in
  up) up "$@" ;;
  down) down ;;
  status) status ;;
  login)
    "$bench" login "${1:-main}"
    echo "the link or the code works once, within one minute; a signed-in browser stays signed in for 30 days" >&2
    ;;
  log) tail -n 40 -f "$log" ;;
  watch) watch ;;
  bench) "$bench" "$@" ;;
  *)
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'
    exit 2
    ;;
esac
