#!/usr/bin/env bash
# The live board: this checkout's board source, served by the Vite dev server
# against the owner's own running daemon, so a board edit shows on real data
# at once (hot reload, no build, no seeding). Each worktree gets its own port,
# so several branches can run side by side.
#
# It acts on the real daemon: a click here is a real answer, the same as on
# the daemon's own board. Use scripts/preview.sh for a seeded bench instead.
#
# Sign-in: a board session is the owner cookie plus a token kept in the
# page origin's own storage, so each live board's origin signs in on its own:
# the owner runs `tiller board --login` in a terminal (an agent cannot) and
# enters the code on the live board. It lasts 30 days, and a worktree keeps
# its port while its board is down, so a link keeps its sign-in. On 127.0.0.1
# every port shares one cookie, so only one loopback live board stays signed
# in at a time; names under TILLER_LIVE_DOMAIN each keep their own.
#
# Usage, inside `nix develop`:
#   scripts/live.sh up [DIR]      start (or reuse) the live board of the worktree at DIR
#                                 (default: the one this script is in); print its link
#   scripts/live.sh down [DIR]    stop that worktree's live board
#   scripts/live.sh down --all    stop every live board
#   scripts/live.sh list          every live board that runs, with its link
#   scripts/live.sh log [DIR]     follow that worktree's dev server log
#
# Environment:
#   TILLER_LIVE_DAEMON  the daemon to use (default http://127.0.0.1:7421)
#   TILLER_LIVE_PORTS   the port range to pick from (default 7440-7459)
#   TILLER_LIVE_DOMAIN  a name whose <port>.<name> a TLS-terminating proxy
#                       sends to this machine's <port>; links then use it over
#                       https. The daemon must allow each such name with
#                       --allow-host, or it answers them 421.
#   TILLER_LIVE_STATE   where the port and pid files live
#                       (default ~/.local/state/tiller-live)
set -euo pipefail

daemon=${TILLER_LIVE_DAEMON:-http://127.0.0.1:7421}
daemon=${daemon%/}
ports=${TILLER_LIVE_PORTS:-7440-7459}
domain=${TILLER_LIVE_DOMAIN:-}
state=${TILLER_LIVE_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/tiller-live}
mkdir -p "$state"

die() {
  echo "live: $*" >&2
  exit 1
}

# The worktree root of DIR.
root_of() {
  git -C "${1:-$(dirname "${BASH_SOURCE[0]}")}" rev-parse --show-toplevel 2>/dev/null ||
    die "not a git checkout: ${1:-.}"
}
# A file-safe name for a worktree root: its branch without the `tiller/`
# prefix (the directory name on a detached head), then a short hash of the
# path, so two worktrees never share one.
name_of() {
  local branch hash
  branch=$(git -C "$1" symbolic-ref --quiet --short HEAD 2>/dev/null || basename "$1")
  branch=${branch#tiller/}
  hash=$(printf '%s' "$1" | shasum | cut -c1-6)
  printf '%s-%s' "${branch//[^A-Za-z0-9._-]/-}" "$hash"
}
# The name of the worktree at DIR; exits when DIR is no checkout.
name_at() {
  local root
  root=$(root_of "${1:-}") || exit 1
  name_of "$root"
}

# Whether NAME's dev server runs: its pid is alive and is still bun, not a
# later process that got the same pid.
alive() {
  local pid
  pid=$(cat "$state/$1.pid" 2>/dev/null) || return 1
  [[ "$(ps -o comm= -p "$pid" 2>/dev/null)" == *bun* ]]
}
port_free() { ! (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
# Whether a running live board other than NAME has PORT.
port_taken() {
  local f other
  for f in "$state"/*.port; do
    [[ -e "$f" ]] || continue
    other=$(basename "$f" .port)
    if [[ "$other" != "$1" && "$(cat "$f")" == "$2" ]] && alive "$other"; then
      return 0
    fi
  done
  return 1
}
link() {
  if [[ -n "$domain" ]]; then
    printf 'https://%s.%s/\n' "$1" "$domain"
  else
    printf 'http://127.0.0.1:%s/\n' "$1"
  fi
}
# What a dev server was started with: a running one is reused only when it
# matches.
settings() { printf '%s %s\n' "$daemon" "$domain"; }

up() {
  local root name port low high p
  root=$(root_of "${1:-}") || exit 1
  name=$(name_of "$root")
  # An older checkout's dev server knows only the fixtures: it would show
  # made-up data under a link that claims to be the real board.
  grep -q TILLER_DAEMON "$root/board/vite.config.ts" ||
    die "$name: this checkout's board/vite.config.ts has no TILLER_DAEMON mode; rebase it on main"
  curl -fsS -m 3 "$daemon/api/v1/health" >/dev/null 2>&1 ||
    die "no daemon answers at $daemon (start it, or set TILLER_LIVE_DAEMON)"

  if alive "$name"; then
    if [[ "$(cat "$state/$name.settings" 2>/dev/null)" == "$(settings)" ]]; then
      echo "$name: $(link "$(cat "$state/$name.port")")"
      return
    fi
    stop "$name" >/dev/null
  fi

  # The port this worktree had before, when it is still free, so a link the
  # owner has open (and signed in on) keeps working; else the first free one
  # that no running live board has.
  port=$(cat "$state/$name.port" 2>/dev/null || true)
  if [[ -z "$port" ]] || ! port_free "$port"; then
    low=${ports%-*} high=${ports#*-} port=
    for ((p = low; p <= high; p++)); do
      if port_free "$p" && ! port_taken "$name" "$p"; then
        port=$p
        break
      fi
    done
    [[ -n "$port" ]] || die "no free port in $ports"
  fi

  # Cheap when nothing changed, and right after a rebase that changed bun.lock.
  bun install --cwd "$root/board" --frozen-lockfile >/dev/null
  echo "$port" >"$state/$name.port"
  echo "$root" >"$state/$name.root"
  settings >"$state/$name.settings"
  (
    cd "$root/board"
    TILLER_DAEMON=$daemon TILLER_LIVE_DOMAIN=$domain nohup \
      bun run dev --host 127.0.0.1 --port "$port" --strictPort \
      >"$state/$name.log" 2>&1 &
    echo $! >"$state/$name.pid"
  )
  # Ready when this server says it listens on the port (another process could
  # hold it and answer a probe), and the port answers.
  for _ in $(seq 1 60); do
    alive "$name" || break
    if grep -q "127.0.0.1:$port/" "$state/$name.log" &&
      curl -fsS -m 1 -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null; then
      echo "$name: $(link "$port")"
      return
    fi
    sleep 0.5
  done
  tail -n 20 "$state/$name.log" >&2
  die "$name: the dev server did not start (log: $state/$name.log)"
}

# Stops NAME's dev server. The port file stays, so the next `up` gets the
# same port (and link) back when it is free.
stop() {
  local name=$1 pid
  if alive "$name"; then
    pid=$(cat "$state/$name.pid")
    # bun runs vite as a child: stop the children, then bun itself.
    pkill -P "$pid" 2>/dev/null || true
    kill "$pid" 2>/dev/null || true
    echo "$name: stopped"
  fi
  rm -f "$state/$name.pid"
}

case "${1:-}" in
  up) up "${2:-}" ;;
  down)
    if [[ "${2:-}" == "--all" ]]; then
      for f in "$state"/*.pid; do
        if [[ -e "$f" ]]; then stop "$(basename "$f" .pid)"; fi
      done
    else
      name=$(name_at "${2:-}") || exit 1
      stop "$name"
    fi
    ;;
  list)
    for f in "$state"/*.pid; do
      name=$(basename "$f" .pid)
      if alive "$name"; then
        # The domain that board was started with, not this shell's.
        domain=$(cut -d' ' -f2- "$state/$name.settings" 2>/dev/null || true)
        echo "$name: $(link "$(cat "$state/$name.port")")  $(cat "$state/$name.root")"
      fi
    done
    ;;
  log)
    name=$(name_at "${2:-}") || exit 1
    tail -f "$state/$name.log"
    ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' && exit 2 ;;
esac
