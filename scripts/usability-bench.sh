#!/usr/bin/env bash
# The usability bench (docs/plan/usability-audit.md): two Tiller homes on one
# machine, paired as main and peer through an SSH shim, with 14 seeded projects
# and `fake-agent` in place of Claude Code. It never reads or writes the real
# ~/.tiller, never uses port 7421 and never talks to a real peer: every command
# runs with an empty environment, a home of its own and a free port.
#
# Usage, inside `nix develop`:
#   scripts/usability-bench.sh up [--no-build]   build, seed and start; print the URLs
#   scripts/usability-bench.sh down              stop every process and delete the homes
#   scripts/usability-bench.sh status            the URLs and which processes live
#   scripts/usability-bench.sh login main|peer   a fresh owner sign-in link for that board
#   scripts/usability-bench.sh main|peer ARGS    `tiller ARGS` on that host, as the owner
#   scripts/usability-bench.sh as HOST SESSION ARGS   `tiller ARGS` as that agent session
#   scripts/usability-bench.sh session HOST PROJECT [CARD]   the live session id of a
#                                                project's orchestrator, or of a card's run
#   scripts/usability-bench.sh daemon HOST stop|start|restart
#   scripts/usability-bench.sh link freeze|thaw  stop or continue the peer's `peer serve`
#   scripts/usability-bench.sh quota HOST PERCENT   the weekly quota the next poll reads
#   scripts/usability-bench.sh kill-agent HOST SESSION   kill that session's agent process
#   scripts/usability-bench.sh shots DIR [PATH...]   screenshots of the main board's pages
#                                                (default: every screen) in both themes at
#                                                1440 px and 390 px, with chrome-devtools-axi.
#                                                Keep DIR outside the repository
#
# The bench lives in $TILLER_BENCH_DIR (default /tmp/tiller-bench; keep it short,
# the holder's socket path has a length limit).
#
# The owner stand-in. A release `tiller` takes an owner command only from a
# terminal with no agent behind it. The bench builds a debug binary with the
# `test-owner` feature of the command tests, and sets TILLER_TEST_OWNER for its
# own homes only, so a script or an agent can walk the owner's journeys here.
# That binary must never be pointed at a real home; `t` below cannot be.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bench=${TILLER_BENCH_DIR:-/tmp/tiller-bench}
bin=$repo/target/debug/tiller
agent=$repo/target/debug/fake-agent
shim=$bench/ssh-shim
alias=bench-peer

case "$bench" in
  /tmp/* | /private/tmp/* | "${TMPDIR:-/nonexistent}"*) ;;
  *) echo "TILLER_BENCH_DIR must be under /tmp or \$TMPDIR: $bench" >&2; exit 2 ;;
esac

git_dir=$(dirname "$(command -v git)")
path="$git_dir:/usr/bin:/bin"

# -- running tiller for one home ---------------------------------------------

# The environment of every bench process of host $1: nothing of the caller's,
# and every home name inside the bench.
bench_env() {
  local root=$bench/$1
  printf '%s\n' \
    "PATH=$path" \
    "HOME=$root/user" \
    "XDG_CONFIG_HOME=$root/user/.config" \
    "XDG_DATA_HOME=$root/user/.local/share" \
    "XDG_STATE_HOME=$root/user/.local/state" \
    "XDG_CACHE_HOME=$root/user/.cache" \
    "CLAUDE_CONFIG_DIR=$root/claude" \
    "TILLER_HOME=$root/home" \
    "TILLER_SSH=$shim" \
    "TILLER_GH=/nonexistent/gh" \
    "TILLER_QUOTA_AXI=$root/quota-axi" \
    "TERM=xterm-256color"
}

# t HOST ARGS: `tiller ARGS` as the owner of that host.
t() {
  local host=$1 vars
  shift
  mapfile -t vars < <(bench_env "$host")
  env -i "${vars[@]}" TILLER_TEST_OWNER=1 "$bin" "$@"
}

# as HOST SESSION ARGS: `tiller ARGS` as an agent session of that host.
as() {
  local host=$1 session=$2 vars
  shift 2
  mapfile -t vars < <(bench_env "$host")
  env -i "${vars[@]}" TILLER_SESSION_ID="$session" "$bin" "$@"
}

# q HOST ARGS: `t`, quiet. A failed seed step stops the bench with its output.
q() {
  local out
  if ! out=$(t "$@" 2>&1); then
    echo "seed step failed: tiller ${*:2}" >&2
    echo "$out" >&2
    exit 1
  fi
}

qas() {
  local out
  if ! out=$(as "$@" 2>&1); then
    echo "seed step failed: tiller ${*:3} (as $2)" >&2
    echo "$out" >&2
    exit 1
  fi
}

# One field of a `--json` response, by a jq path.
field() { jq -r "$1"; }

# -- processes ---------------------------------------------------------------

# Starts `tiller ARGS` for HOST detached, so no agent that runs this script is
# among its ancestors (`peer serve` refuses one, authority.md G44). Its output
# goes to $bench/HOST/NAME.out and its pid to NAME.pid.
detached() {
  local host=$1 name=$2 vars
  shift 2
  mapfile -t vars < <(bench_env "$host")
  /bin/sh -c 'out=$1; pid=$2; shift 2; "$@" >"$out" 2>"$out.err" </dev/null & echo $! >"$pid"' sh \
    "$bench/$host/$name.out" "$bench/$host/$name.pid" \
    env -i "${vars[@]}" TILLER_LOG=warn "$bin" "$@"
}

alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

stop_pid() {
  local file=$1 pid
  [ -f "$file" ] || return 0
  pid=$(cat "$file")
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 50); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$file"
}

start_holder() {
  local host=$1
  alive "$bench/$host/holder.pid" && return 0
  detached "$host" holder holder
  for _ in $(seq 1 100); do
    [ -S "$bench/$host/home/run/holder.sock" ] && return 0
    sleep 0.1
  done
  echo "the holder of $host made no socket" >&2
  exit 1
}

start_daemon() {
  local host=$1 port=0
  alive "$bench/$host/daemon.pid" && return 0
  # A restart keeps the port, so an open board tab finds the daemon again.
  [ -f "$bench/$host/port" ] && port=$(cat "$bench/$host/port")
  : >"$bench/$host/daemon.out"
  detached "$host" daemon daemon --port "$port"
  for _ in $(seq 1 150); do
    url=$(sed -n 's/^listening on //p' "$bench/$host/daemon.out")
    if [ -n "$url" ]; then
      echo "${url##*:}" >"$bench/$host/port"
      return 0
    fi
    sleep 0.1
  done
  echo "the daemon of $host reported no address:" >&2
  cat "$bench/$host/daemon.out.err" >&2
  exit 1
}

url() { echo "http://127.0.0.1:$(cat "$bench/$1/port")"; }

login_link() {
  local link
  link=$(t "$1" board --login 2>&1 | grep -o '/login/[0-9a-f]*' | head -n 1)
  [ -n "$link" ] || { echo "no login link from $1" >&2; exit 1; }
  echo "$(url "$1")$link"
}

# -- the fixed parts of a home -----------------------------------------------

write_shim() {
  # What the main host's daemon and `tiller peer add` run as `ssh`: it drops
  # the options, takes the alias and runs tiller for the peer's home, the way
  # sshd would on the other machine.
  local vars
  mapfile -t vars < <(bench_env peer)
  {
    echo '#!/bin/sh'
    echo 'while [ "$#" -gt 0 ]; do'
    echo '  case "$1" in'
    echo '    -o) shift 2 ;;'
    echo '    -T|-t) shift ;;'
    echo '    --) shift; break ;;'
    echo '    *) break ;;'
    echo '  esac'
    echo 'done'
    echo "[ \"\$1\" = $alias ] || exit 255"
    echo 'shift'
    echo '[ "$1" = tiller ] || exit 127'
    echo 'shift'
    echo "[ \"\$1 \$2\" = \"peer serve\" ] && echo \$\$ >\"$bench/serve.pid\""
    printf 'exec env -i'
    printf ' %q' "${vars[@]}" 'SSH_CONNECTION=192.0.2.1 50000 192.0.2.2 22' TILLER_TEST_OWNER=1 "$bin"
    echo ' "$@"'
  } >"$shim"
  chmod +x "$shim"
}

write_stubs() {
  local host=$1 root=$bench/$1
  mkdir -p "$root"/{user,claude,pool,scratch,orchestrators,checkouts,origins,agent-logs}
  # Claude Code's stand-in: the SessionStart hook Claude Code would fire, then
  # fake-agent, whose turns run 4 s.
  cat >"$root/claude-stub" <<STUB
#!/bin/sh
if [ "\$1" = --version ]; then exec "$agent" --version; fi
id=claude-\$TILLER_SESSION_ID
case " \$* " in *" --resume "*) id=\$id-resumed;; esac
printf '{"session_id":"%s","source":"startup"}' "\$id" | "$bin" hook SessionStart >/dev/null 2>&1
exec env FAKE_AGENT_LOG="$root/agent-logs/\$TILLER_SESSION_ID.jsonl" FAKE_AGENT_BUSY_MS=4000 "$agent" "\$@"
STUB
  # quota-axi's stand-in: one weekly window, with the percent left that
  # `usability-bench.sh quota` wrote.
  echo 64 >"$root/quota-percent"
  cat >"$root/quota-axi" <<STUB
#!/bin/sh
provider=claude
while [ "\$#" -gt 0 ]; do
  case "\$1" in --provider) provider=\$2; shift 2 ;; *) shift ;; esac
done
left=\$(cat "$root/quota-percent")
now=\$(date -u +%Y-%m-%dT%H:%M:%SZ)
reset=\$(date -u -v+3d +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+3 days' +%Y-%m-%dT%H:%M:%SZ)
cat <<JSON
{"schemaVersion":5,"generatedAt":"\$now","providers":[{"provider":"\$provider",
 "state":{"status":"fresh","refreshedAt":"\$now"},
 "windows":[{"id":"seven_day","kind":"weekly","label":"week","windowSeconds":604800,
  "percentRemaining":\$left,"resetsAt":"\$reset",
  "pace":{"status":"on_track","reservePercentPoints":6.5,"burnMultiple":0.9,"elapsedPercent":57.0}},
 {"id":"five_hour","kind":"session","label":"5h","windowSeconds":18000,
  "percentRemaining":81,"resetsAt":"\$reset"}]}]}
JSON
STUB
  # treehouse's stand-in (the worktree pool): `get` cuts a git worktree of the
  # checkout it runs in under the pool root, `return` removes it.
  cat >"$root/treehouse" <<STUB
#!/bin/sh
case "\$1" in
  get)
    while [ "\$#" -gt 0 ]; do
      case "\$1" in
        --branch) branch=\$2; shift 2 ;;
        --base) base=\$2; shift 2 ;;
        *) shift ;;
      esac
    done
    tree="$root/pool/\$(basename "\$PWD")-\$(echo "\$branch" | tr / -)"
    git worktree add -q -b "\$branch" "\$tree" "\$base" >&2 || exit 1
    echo "\$tree"
    ;;
  return) git -C "\$2" worktree remove --force "\$2" >&2 ;;
  *) echo "treehouse stand-in 0.0.0" ;;
esac
STUB
  chmod +x "$root/claude-stub" "$root/quota-axi" "$root/treehouse"
}

init_home() {
  local host=$1 label=$2 root=$bench/$1
  q "$host" init --host-label "$label" --lane-capacity 12
  for pair in pool_root:pool scratch_root:scratch orchestrator_home:orchestrators checkouts_root:checkouts; do
    q "$host" config set "spawn.${pair%%:*}" "$root/${pair##*:}"
  done
  q "$host" config set spawn.claude "$root/claude-stub"
  q "$host" config set spawn.treehouse "[\"$root/treehouse\"]"
}

# project HOST ID NAME: a local-only project with a checkout of its own.
project() {
  local host=$1 id=$2 name=$3 checkout=$bench/$1/checkouts/$2
  q "$host" project add "$id" --name "$name" --delivery local-only --capacity 3
  # Its origin is a bare repository beside it: a run starts from origin's head.
  git init -q --bare -b main "$bench/$host/origins/$id.git"
  git init -q -b main "$checkout"
  echo "# $name" >"$checkout/README.md"
  git -C "$checkout" add README.md
  git -C "$checkout" -c user.name=Bench -c user.email=bench@example.com commit -q -m "Start $name"
  git -C "$checkout" remote add origin "$bench/$host/origins/$id.git"
  git -C "$checkout" push -q -u origin main
  q "$host" config set "checkouts.$id" "$checkout"
}

# card HOST PROJECT KIND TITLE [ARGS]: prints the new card's display id.
card() {
  local host=$1 project=$2 kind=$3 title=$4
  shift 4
  t "$host" --json card create "$project" --kind "$kind" --title "$title" "$@" | field .data.display_id
}

orchestrator() {
  t "$1" --json orchestrator start "$2" | field .data.session.id
}

# session HOST PROJECT [CARD]: the live session of the project's orchestrator,
# or of the card's newest run.
session() {
  local host=$1 project=$2 card=${3:-}
  if [ -z "$card" ]; then
    t "$host" --json agents | jq -r --arg p "$project" \
      '.data.agents[] | select(.orchestrator.project_id == $p) | .orchestrator.session_id'
  else
    t "$host" --json agents | jq -r --arg c "$card" \
      '.data.agents[].workers[] | select(.card == $c) | .session_id' | head -n 1
  fi
}

# -- the commands ------------------------------------------------------------

build() {
  (
    cd "$repo"
    cargo build -p tiller-cli --features tiller-core/test-owner
    cargo build -p tiller-panes --features testing --bin fake-agent
    [ -d board/node_modules ] || bun install --cwd board --frozen-lockfile
    bun run --cwd board build
  )
}

down() {
  [ -d "$bench" ] || return 0
  for host in main peer; do
    stop_pid "$bench/$host/daemon.pid"
    stop_pid "$bench/$host/holder.pid"
  done
  # The peer's `peer serve` ends with the main daemon's pipe; a frozen one
  # must run again to see that. The agents end with their holder.
  [ -f "$bench/serve.pid" ] && kill -CONT "$(cat "$bench/serve.pid")" 2>/dev/null || true
  rm -rf "$bench"
}

status() {
  [ -d "$bench" ] || { echo "the bench is down"; return 0; }
  for host in main peer; do
    for name in daemon holder; do
      if alive "$bench/$host/$name.pid"; then state=up; else state=DOWN; fi
      echo "$host $name: $state"
    done
    [ -f "$bench/$host/port" ] && echo "$host board: $(url "$host")"
  done
  return 0
}

up() {
  [ "${1:-}" = --no-build ] || build
  [ -x "$bin" ] && [ -x "$agent" ] || { echo "build first: $bin and $agent" >&2; exit 1; }
  [ -d "$repo/board/dist" ] || { echo "board/dist is missing: bun run --cwd board build" >&2; exit 1; }
  down
  mkdir -p "$bench"
  write_stubs main
  write_stubs peer
  write_shim
  init_home main main
  init_home peer peer
  start_holder peer
  start_holder main
  start_daemon peer
  start_daemon main
  # Pair: the owner types y at the peer's prompt.
  echo y | t main peer add peer --ssh "$alias" >"$bench/pair.out" 2>&1 ||
    { echo "pairing failed:" >&2; cat "$bench/pair.out" >&2; exit 1; }
  # shellcheck source=/dev/null
  . "$repo/scripts/usability-bench-seed.sh"
  seed
  # A binary that cannot find the board build would give every screenshot a
  # false verdict.
  for host in main peer; do
    curl -fsS --max-time 10 "$(url "$host")/" | grep -q '<div id="root">' ||
      { echo "the daemon of $host does not serve the board: build it (bun run --cwd board build)" >&2; exit 1; }
  done
  echo
  status
  echo
  echo "main board (signed in): $(login_link main)"
  echo "peer board (signed in): $(login_link peer)"
  echo "stop it with: scripts/usability-bench.sh down"
}

# shots DIR [PATH...]: DIR/<page>-<width>-<theme>.png for each page of the
# main board. chrome-devtools-axi is the owner's own tool, not the dev shell's.
shots() {
  local dir=${1:?a directory outside the repository} base page name spec theme
  shift
  command -v chrome-devtools-axi >/dev/null || { echo "chrome-devtools-axi is not on PATH" >&2; exit 1; }
  case "$(cd "$(dirname "$dir")" && pwd)/" in
    "$repo"/*) echo "keep screenshots out of the repository: $dir" >&2; exit 2 ;;
  esac
  mkdir -p "$dir"
  export CHROME_DEVTOOLS_AXI_SESSION=tiller-bench
  base=$(url main)
  [ $# -gt 0 ] || set -- / /inbox /agents /contributions /memory /health /settings
  # Sign this browser in once; the session cookie covers every later page.
  chrome-devtools-axi open "$(login_link main)" >/dev/null
  for page in "$@"; do
    name=$(echo "${page#/}" | tr -c 'a-zA-Z0-9\n' '-')
    [ -n "${name%-}" ] || name=board
    for spec in "1440:1440x900x1" "390:390x844x2,mobile,touch"; do
      for theme in light dark; do
        chrome-devtools-axi emulate --viewport "${spec#*:}" --color-scheme "$theme" >/dev/null
        chrome-devtools-axi open "$base$page" >/dev/null
        chrome-devtools-axi wait 700 >/dev/null
        chrome-devtools-axi screenshot "$dir/${name%-}-${spec%%:*}-$theme.png" >/dev/null
      done
    done
  done
  echo "screenshots in $dir"
}

command=${1:-}
[ $# -gt 0 ] && shift
case "$command" in
  up) up "$@" ;;
  down) down ;;
  status) status ;;
  login) login_link "${1:?main or peer}" ;;
  main | peer) t "$command" "$@" ;;
  as) as "$@" ;;
  session) session "$@" ;;
  daemon)
    host=${1:?main or peer}
    case "${2:?stop, start or restart}" in
      stop) stop_pid "$bench/$host/daemon.pid" ;;
      start) start_daemon "$host" ;;
      restart) stop_pid "$bench/$host/daemon.pid"; start_daemon "$host" ;;
      *) echo "daemon HOST stop|start|restart" >&2; exit 2 ;;
    esac
    ;;
  link)
    pid=$(cat "$bench/serve.pid")
    case "${1:?freeze or thaw}" in
      freeze) kill -STOP "$pid" ;;
      thaw) kill -CONT "$pid" ;;
      *) echo "link freeze|thaw" >&2; exit 2 ;;
    esac
    ;;
  shots) shots "$@" ;;
  quota) echo "${2:?percent left}" >"$bench/${1:?main or peer}/quota-percent" ;;
  kill-agent)
    log=$bench/${1:?main or peer}/agent-logs/${2:?session}.jsonl
    kill "$(jq -r 'select(.event == "start") | .pid' "$log" | tail -n 1)"
    ;;
  *)
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'
    exit 2
    ;;
esac
