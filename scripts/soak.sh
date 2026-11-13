#!/usr/bin/env bash
# The terminal holder's soak (spec 13, row H14): fake-agent sessions on a
# test home with a daemon, with daemon restarts, 3 live upgrades and a holder
# kill over the given time, and a counter check and the daemon hooks after
# each event. See crates/cli/tests/soak.rs.
#
#   scripts/soak.sh            4 h (14400 s), the real H14 run
#   scripts/soak.sh 600        10 min, the same schedule scaled down
#
# Run it inside `nix develop`. The report is written to $TILLER_SOAK_OUT
# (default target/tmp/soak/long): soak-result.json is rewritten after every
# event, so a stopped run leaves its progress, and soak-report.md is the
# readable form. The exit code is that of the test: 0 means no gap.
set -euo pipefail

cd "$(dirname "$0")/.."
export TILLER_SOAK_SECS="${1:-14400}"
export TILLER_SOAK_OUT="${TILLER_SOAK_OUT:-$PWD/target/tmp/soak/long}"
mkdir -p "$TILLER_SOAK_OUT"
echo "soak: ${TILLER_SOAK_SECS} s, report in ${TILLER_SOAK_OUT}" >&2
# A run of one package does not build the stand-in for Claude Code.
cargo build -p tiller-panes --features testing --bin fake-agent
exec cargo nextest run -p tiller-cli --test soak --run-ignored only \
  --no-capture -E 'test(the_long_soak)'
