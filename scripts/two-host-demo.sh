#!/usr/bin/env bash
# Two Tiller homes on one machine, joined through an SSH shim, so you can use the
# federated board by hand (ADR 0003, protocol.md section 10): see the peer's card,
# answer its question from the board host, freeze the peer, watch the staleness.
#
# Usage: scripts/two-host-demo.sh <path-to-tiller-binary>
#
# Run it yourself in a terminal, not from an agent: `tiller peer add` and `peer
# accept` need the owner at a terminal and ask you to confirm once (type y).
# Everything lives in a temporary directory that is removed on exit.
set -euo pipefail

bin=$(readlink -f "${1:?usage: two-host-demo.sh <path-to-tiller-binary>}")
work=$(mktemp -d)
board_home=$work/board
peer_home=$work/peer
mkdir -p "$board_home" "$peer_home"
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

# What the board host's daemon and `tiller peer add` run as `ssh`: it drops the
# options, takes the alias and runs tiller for the peer's home, the way sshd would
# on the other machine. The pid of `peer serve` goes to $work/serve.pid, so the
# peer can be frozen with `kill -STOP`.
shim=$work/ssh-shim
cat >"$shim" <<SHIM
#!/bin/sh
while [ "\$#" -gt 0 ]; do
  case "\$1" in
    -o) shift 2 ;;
    -T|-t) shift ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[ "\$1" = peer-alias ] || exit 255
shift
[ "\$1" = tiller ] || exit 127
shift
[ "\$1 \$2" = "peer serve" ] && echo \$\$ > "$work/serve.pid"
export TILLER_HOME="$peer_home" TILLER_HERDR=/nonexistent/herdr
export SSH_CONNECTION="192.0.2.1 50000 192.0.2.2 22"
exec "$bin" "\$@"
SHIM
chmod +x "$shim"

on_board() { TILLER_HOME=$board_home TILLER_HERDR=/nonexistent/herdr TILLER_SSH=$shim "$bin" "$@"; }
on_peer() { TILLER_HOME=$peer_home TILLER_HERDR=/nonexistent/herdr "$bin" "$@"; }

on_board init --host-label board-host --lane-capacity 2 >/dev/null
on_peer init --host-label peer-host --lane-capacity 2 >/dev/null
on_board project add alpha --name Alpha --repo example/alpha --delivery direct-pr --capacity 2 >/dev/null
on_peer project add beta --name Beta --repo example/beta --delivery direct-pr --capacity 2 >/dev/null

on_board card create alpha --kind ship --title "A card on the board host" >/dev/null
card=$(on_peer --json card create beta --kind ship --title "A card on the peer" | sed -n 's/.*"display_id":"\([^"]*\)".*/\1/p' | head -n 1)
on_peer attention open "$card" --kind decision --question "Which database should the peer use?" >/dev/null

# The daemons. Run from a terminal, so no Claude Code process is an ancestor of
# `peer serve` (authority.md G44).
start_daemon() {
  local home=$1 log=$2
  TILLER_HOME=$home TILLER_HERDR=/nonexistent/herdr TILLER_QUOTA_AXI=/nonexistent/quota-axi \
    TILLER_SSH=$shim TILLER_LOG=warn "$bin" daemon --port 0 >"$log" 2>"$log.err" &
  pids+=("$!")
  for _ in $(seq 1 100); do
    url=$(sed -n 's/^listening on //p' "$log")
    [ -n "$url" ] && { echo "$url"; return; }
    sleep 0.1
  done
  echo "daemon did not report an address" >&2
  exit 1
}
peer_url=$(start_daemon "$peer_home" "$work/peer.log")
board_url=$(start_daemon "$board_home" "$work/board.log")

echo "Pairing: confirm on the peer with y."
on_board peer add peer --ssh peer-alias

for _ in $(seq 1 100); do
  curl -fsS "$board_url/api/v1/snapshot" | grep -q '"link":"live"' && break
  sleep 0.2
done
curl -fsS "$board_url/api/v1/snapshot" | grep -q '"link":"live"' || { echo "the link never went live" >&2; exit 1; }

login=$(on_board board --login | grep -o '/login/[0-9a-f]*' | head -n 1)
cat <<EOF

Board host:  $board_url${login}   (opens the board signed in as the owner)
Peer's own:  $peer_url

Try, in the board host's browser:
  1. Open the card "A card on the peer" and answer "Which database should the peer use?".
     The answer is recorded on the peer, not here.
  2. Freeze the peer:      kill -STOP \$(cat $work/serve.pid)
     After about 25 s its rows dim and say "as of"; your own card and writes stay live.
  3. Thaw it:              kill -CONT \$(cat $work/serve.pid)

Press Enter to stop both daemons and delete the temporary homes.
EOF
read -r _
