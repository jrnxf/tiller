#!/usr/bin/env bash
# tiller-probe.sh - read-only host capability probe for Tiller.
#
# It answers what a host lets Tiller do: hooks under managed settings, a
# user service, a localhost port, herdr, auto memory, the claude launch
# flags, plugin evals, sqlite3, Nix store binaries and transcript paths.
#
# Everything it creates lives under one temp root. Claude Code runs either
# with CLAUDE_CONFIG_DIR inside that root (offline tier, no login, no cost)
# or, for the live tier, with the real login and the memory directory
# redirected into the root. A before and after manifest of the user's
# configuration, plus a scan for anything named after this run, proves
# nothing else changed. See probe/README.md.
#
# Works with macOS /bin/bash 3.2 and Linux bash. jq is optional.

# Markdown backticks in printf formats are literal.
# shellcheck disable=SC2016

set -u

# The whole script is one brace group, so bash parses all of it before
# running any of it. When it is piped into `bash -s`, a child that reads
# stdin cannot eat the rest of the script; stdin is then closed.
{
exec </dev/null
# fd 3 keeps the original stderr for the interrupt handler, which runs under
# the redirections of whatever command it interrupted.
exec 3>&2

PROBE_VERSION=1

usage() {
	cat <<'EOF'
Usage: tiller-probe.sh [options]

  --host-role ROLE   label for the report (personal-mac, remote-linux, work-mac)
  --out DIR          write report.json, report.md and proof files to DIR
                     (default: a new directory under $TMPDIR)
  --offline          skip the live tier (no login, no model calls, no cost)
  --no-service-trial skip loading a throwaway launchd agent or systemd unit
  --tiller PATH      also run this tiller binary (the T0.1 build)
  --nix-hello        build nixpkgs#hello with nix and run it (adds to the
                     Nix store, so it is off by default)
  --keep-work        keep the scratch directory for debugging
  -h, --help         show this help

The live tier makes three small haiku calls (about USD 0.06).
EOF
}

HOST_ROLE=""
OUT_DIR=""
OFFLINE=0
SERVICE_TRIAL=1
TILLER_BIN=""
NIX_HELLO=0
KEEP_WORK=0

# need_value OPTION ARGC - an option that takes a value must have one.
need_value() {
	if [ "$2" -lt 2 ]; then
		echo "tiller-probe: $1 needs a value" >&2
		exit 2
	fi
}

while [ $# -gt 0 ]; do
	case "$1" in
	--host-role) need_value "$1" $#; HOST_ROLE="$2"; shift 2 ;;
	--out) need_value "$1" $#; OUT_DIR="$2"; shift 2 ;;
	--offline) OFFLINE=1; shift ;;
	--no-service-trial) SERVICE_TRIAL=0; shift ;;
	--tiller) need_value "$1" $#; TILLER_BIN="$2"; shift 2 ;;
	--nix-hello) NIX_HELLO=1; shift ;;
	--keep-work) KEEP_WORK=1; shift ;;
	-h | --help) usage; exit 0 ;;
	*) echo "tiller-probe: unknown option: $1" >&2; usage >&2; exit 2 ;;
	esac
done

# ---------------------------------------------------------------- setup

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
Darwin) PLATFORM=macos ;;
Linux) PLATFORM=linux ;;
*) PLATFORM=other ;;
esac
[ -n "$HOST_ROLE" ] || HOST_ROLE="unlabelled-$PLATFORM"

have() { command -v "$1" >/dev/null 2>&1; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/tiller-probe-XXXXXX")" || {
	echo "tiller-probe: mktemp failed" >&2
	exit 1
}
ROOT="$(cd "$ROOT" && pwd -P)"
RUN_ID="$(basename "$ROOT")"
RUN_TAG="$(printf '%s' "$RUN_ID" | tr '[:upper:]' '[:lower:]')"
W="$ROOT/work"
mkdir -p "$W" "$W/tmp"
if [ -z "$OUT_DIR" ]; then
	OUT_DIR="$ROOT/report"
fi
mkdir -p "$OUT_DIR" || exit 1
OUT_DIR="$(cd "$OUT_DIR" && pwd -P)"

LIVE_CFG="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
if [ "$PLATFORM" = macos ]; then
	CLAUDE_CACHE="$HOME/Library/Caches/claude-cli-nodejs"
else
	CLAUDE_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/claude-cli-nodejs"
fi
if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
	LIVE_CLAUDE_JSON="$CLAUDE_CONFIG_DIR/.claude.json"
else
	LIVE_CLAUDE_JSON="$HOME/.claude.json"
fi

log() { printf 'tiller-probe: %s\n' "$*" >&2; }

new_uuid() {
	if have uuidgen; then
		uuidgen | tr '[:upper:]' '[:lower:]'
	elif [ -r /proc/sys/kernel/random/uuid ]; then
		cat /proc/sys/kernel/random/uuid
	else
		od -An -N16 -tx1 /dev/urandom | tr -d ' \n' |
			sed 's/^\(.\{8\}\)\(.\{4\}\).\(.\{3\}\).\(.\{3\}\)\(.\{12\}\)$/\1-\2-4\3-a\4-\5/'
	fi
}

sha() {
	if have shasum; then
		shasum -a 256 | cut -c1-64
	else
		sha256sum | cut -c1-64
	fi
}

# run_timeout SECONDS CMD... - portable timeout (macOS has no timeout(1)).
run_timeout() {
	local secs="$1"
	shift
	if have perl; then
		perl -e 'alarm shift; exec @ARGV or exit 127' "$secs" "$@"
	else
		"$@"
	fi
}

# The slug Claude Code uses for a project directory under projects/.
slug_of() { printf '%s' "$1" | sed 's/[^a-zA-Z0-9]/-/g'; }

# Session-linking variables from a parent Claude Code session; a child
# claude must not inherit them. Provider and auth variables are kept.
CLEAN_ENV="-u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_ENTRYPOINT
 -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_MESSAGING_SOCKET
 -u CLAUDE_CODE_MESSAGING_TOKEN -u CLAUDE_CODE_BRIDGE_SESSION_ID
 -u CLAUDE_CODE_SESSION_ATTENDED -u CLAUDE_PID -u CLAUDE_EFFORT
 -u CLAUDE_CODE_DISABLE_AUTO_MEMORY"

# claude_in DIR [NAME=VALUE...] -- CLAUDE_ARGS...
# Runs claude in DIR with a clean session environment, no stdin and a
# 150 s timeout. env(1) cannot run a shell function, so the timeout is
# inlined rather than going through run_timeout.
claude_in() {
	local dir="$1"
	shift
	local assigns=()
	while [ $# -gt 0 ] && [ "$1" != "--" ]; do
		assigns[${#assigns[@]}]="$1"
		shift
	done
	[ "${1:-}" = "--" ] && shift
	local timeout=()
	have perl && timeout=(perl -e 'alarm(shift); exec(@ARGV) or exit(127)' 150)
	# shellcheck disable=SC2086
	(cd "$dir" && env $CLEAN_ENV ${CLAUDE_EXTRA_UNSET:-} "TMPDIR=$W/tmp" ${assigns[@]+"${assigns[@]}"} ${timeout[@]+"${timeout[@]}"} claude "$@" </dev/null)
}

# Offline tier: a config dir inside the temp root and no provider
# credentials in the environment. Managed settings can still supply an env
# block or an apiKeyHelper; the offline checks report when a run
# authenticated anyway, and --max-budget-usd caps what it could spend.
OFFLINE_UNSET="-u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN -u CLAUDE_CODE_OAUTH_TOKEN
 -u ANTHROPIC_FOUNDRY_API_KEY -u CLAUDE_CODE_USE_BEDROCK -u CLAUDE_CODE_USE_VERTEX
 -u CLAUDE_CODE_USE_FOUNDRY"

# claude_offline DIR CONFIG_DIR [NAME=VALUE...] -- CLAUDE_ARGS...
claude_offline() {
	local dir="$1" cfg="$2"
	shift 2
	CLAUDE_EXTRA_UNSET="$OFFLINE_UNSET" claude_in "$dir" "CLAUDE_CONFIG_DIR=$cfg" \
		"ANTHROPIC_BASE_URL=http://127.0.0.1:9" "$@"
}

# ---------------------------------------------------------------- results

# Each check appends one record: id, status, summary, then key=value pairs.
N=0
R_ID=()
R_STATUS=()
R_SUMMARY=()
R_DETAILS=()

record() {
	local id="$1" status="$2" summary="$3"
	shift 3
	local d=""
	while [ $# -gt 0 ]; do
		d="$d$1"$'\x1f'
		shift
	done
	R_ID[N]="$id"
	R_STATUS[N]="$status"
	R_SUMMARY[N]="$summary"
	R_DETAILS[N]="$d"
	N=$((N + 1))
	log "$status  $id - $summary"
}

json_str() {
	# Escape a string for JSON: backslash, quote, control characters.
	printf '%s' "$1" | perl -0777 -pe 's/\\/\\\\/g; s/"/\\"/g; s/\n/\\n/g; s/\r/\\r/g; s/\t/\\t/g; s/([\x00-\x1f])/sprintf("\\u%04x", ord($1))/ge' 2>/dev/null ||
		printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n\t' '  '
}

json_val() {
	case "$1" in
	true | false | null) printf '%s' "$1" ;;
	*)
		if printf '%s' "$1" | grep -Eq '^-?(0|[1-9][0-9]{0,15})$'; then
			printf '%s' "$1"
		else
			printf '"%s"' "$(json_str "$1")"
		fi
		;;
	esac
}

# Redact machine-identifying values so a report can be pasted safely.
redact() {
	H="$HOME" RT="$ROOT" OD="$OUT_DIR" U="$(id -un)" perl -pe '
		s/\Q$ENV{OD}\E/\$PROBE_REPORT/g if length $ENV{OD};
		s/\Q$ENV{RT}\E/\$PROBE_ROOT/g if length $ENV{RT};
		s/\Q$ENV{H}\E/~/g if length $ENV{H};
		s{/\Q$ENV{U}\E(?=/|\b)}{/<user>}g if length $ENV{U};
	'
}

# ---------------------------------------------------------------- proof

PROBE_SIDS=""

# manifest - one line per thing the probe could plausibly change. Volatile
# files Claude Code rewrites on every start (the body of .claude.json, its
# backups, telemetry) are excluded; .claude.json is checked for references
# to this run instead.
manifest() {
	local p
	for p in \
		"$LIVE_CFG/settings.json" \
		"$LIVE_CFG/settings.local.json" \
		"$LIVE_CFG/CLAUDE.md" \
		"$LIVE_CFG/keybindings.json" \
		"$LIVE_CFG/plugins/installed_plugins.json" \
		"$LIVE_CFG/plugins/known_marketplaces.json" \
		"/Library/Application Support/ClaudeCode/managed-settings.json" \
		"/Library/Application Support/ClaudeCode/managed-mcp.json" \
		"/etc/claude-code/managed-settings.json" \
		"/etc/claude-code/managed-mcp.json" \
		"$HOME/.gitconfig" \
		"$HOME/.config/git/config" \
		"$HOME/.config/herdr/config.toml"; do
		if [ -f "$p" ]; then
			printf 'file %s %s\n' "$p" "$(sha <"$p")"
		else
			printf 'absent %s\n' "$p"
		fi
	done
	for p in \
		"/Library/Application Support/ClaudeCode/managed-settings.d" \
		"/etc/claude-code/managed-settings.d" \
		"$HOME/Library/LaunchAgents" \
		"$HOME/.config/systemd/user"; do
		if [ -d "$p" ]; then
			find "$p" \( -type f -o -type l \) 2>/dev/null | LC_ALL=C sort | while IFS= read -r f; do
				if [ -f "$f" ]; then
					printf 'file %s %s\n' "$f" "$(sha <"$f" 2>/dev/null)"
				else
					printf 'link %s\n' "$f"
				fi
			done
			printf 'dir %s\n' "$p"
		else
			printf 'absent %s\n' "$p"
		fi
	done
	printf 'crontab %s\n' "$(crontab -l 2>/dev/null | sha)"
	if [ -f "$LIVE_CLAUDE_JSON" ]; then
		printf 'claude.json-refs-to-run %s\n' "$(grep -c -i -F "$RUN_ID" "$LIVE_CLAUDE_JSON" 2>/dev/null)"
	fi
	if [ "$PLATFORM" = macos ] && have launchctl; then
		printf 'launchd-probe-jobs %s\n' "$(launchctl list 2>/dev/null | grep -c 'tiller-probe')"
	fi
	if [ "$PLATFORM" = linux ] && have systemctl; then
		printf 'systemd-probe-units %s\n' "$(systemctl --user list-units --all --no-legend 2>/dev/null | grep -c 'tiller-probe')"
	fi
}

# leftovers - anything under the Claude config or cache dir whose name
# carries this run's id (project slugs, plugin data dirs) or one of its
# session ids.
leftovers() {
	local pat="-iname *$RUN_ID*"
	local s d
	for s in $PROBE_SIDS; do
		pat="$pat -o -name *$s*"
	done
	for d in "$LIVE_CFG" "$CLAUDE_CACHE"; do
		[ -d "$d" ] || continue
		set -f
		# shellcheck disable=SC2086
		find "$d" -maxdepth 3 \( $pat \) 2>/dev/null
		set +f
	done | LC_ALL=C sort
}

# Remove what this run's live sessions created under the real config and
# cache dirs: exactly the paths leftovers lists. Each name carries this
# run's random id or one of its random session ids, so nothing that existed
# before the run can match; the guard re-checks that before each removal.
cleanup_live() {
	local p base s ok
	leftovers | while IFS= read -r p; do
		[ -e "$p" ] || continue
		case "$p" in "$LIVE_CFG"/* | "$CLAUDE_CACHE"/*) ;; *) continue ;; esac
		base="$(basename "$p" | tr '[:upper:]' '[:lower:]')"
		ok=0
		case "$base" in *"$RUN_TAG"*) ok=1 ;; esac
		for s in $PROBE_SIDS; do
			case "$base" in *"$s"*) ok=1 ;; esac
		done
		[ "$ok" = 1 ] && rm -rf "$p"
	done
}

# An interrupted run still unloads its launchd agent, removes what its live
# sessions created and deletes its scratch directory. No report is written.
SERVICE_UNLOAD=""
on_abort() {
	trap - INT TERM HUP
	log "interrupted; cleaning up" 2>&3
	[ -n "$SERVICE_UNLOAD" ] && launchctl bootout "$SERVICE_UNLOAD" >/dev/null 2>&1
	cleanup_live
	rm -rf "$W"
	case "$OUT_DIR" in "$ROOT"/*) ;; *) rmdir "$ROOT" 2>/dev/null ;; esac
	exit 130
}
trap on_abort INT TERM HUP

START_MARK="$W/.start"
: >"$START_MARK"
manifest >"$OUT_DIR/manifest-before.txt"
leftovers >"$OUT_DIR/leftovers-before.txt"

# ---------------------------------------------------------------- checks

check_env() {
	local claude_v="missing" nix_v="missing" herdr_pane=false
	have claude && claude_v="$(claude --version 2>/dev/null | head -1)"
	have nix && nix_v="$(nix --version 2>/dev/null | head -1)"
	[ -n "${HERDR_PANE_ID:-}" ] && herdr_pane=true
	local osv="$OS"
	if [ "$PLATFORM" = macos ]; then
		osv="macOS $(sw_vers -productVersion 2>/dev/null)"
	elif [ -r /etc/os-release ]; then
		# shellcheck disable=SC1091
		osv="$(. /etc/os-release && printf '%s' "${PRETTY_NAME:-Linux}")"
	fi
	record env info "$osv $ARCH, claude $claude_v" \
		"host_role=$HOST_ROLE" "platform=$PLATFORM" "os=$osv" "arch=$ARCH" \
		"bash=$BASH_VERSION" "claude=$claude_v" "nix=$nix_v" "jq=$(have jq && echo true || echo false)" \
		"inside_herdr_pane=$herdr_pane" "claude_config_dir_set=$([ -n "${CLAUDE_CONFIG_DIR:-}" ] && echo true || echo false)"
}

# Read one top-level key from a managed settings source without printing
# anything else from it (it may hold env values).
managed_key() {
	local file="$1" key="$2"
	case "$file" in
	*.plist)
		plutil -extract "$key" raw -o - "$file" 2>/dev/null || true
		;;
	*)
		if have jq; then
			jq -r --arg k "$key" 'if has($k) then .[$k]|tostring else empty end' "$file" 2>/dev/null || true
		elif [ "$PLATFORM" = macos ]; then
			plutil -extract "$key" raw -o - "$file" 2>/dev/null || true
		else
			grep -o "\"$key\"[[:space:]]*:[[:space:]]*[a-z]*" "$file" 2>/dev/null | head -1 | sed 's/.*:[[:space:]]*//'
		fi
		;;
	esac
}

MANAGED_HOOKS_ONLY="unset"
MANAGED_DISABLE_ALL="unset"

check_managed_settings() {
	local sources=""
	local f
	local list=""
	if [ "$PLATFORM" = macos ]; then
		list="/Library/Application Support/ClaudeCode/managed-settings.json"
		for f in "/Library/Application Support/ClaudeCode/managed-settings.d/"*.json; do
			[ -f "$f" ] && list="$list"$'\n'"$f"
		done
		list="$list"$'\n'"/Library/Managed Preferences/com.anthropic.claudecode.plist"
		list="$list"$'\n'"/Library/Managed Preferences/$(id -un)/com.anthropic.claudecode.plist"
	else
		list="/etc/claude-code/managed-settings.json"
		for f in /etc/claude-code/managed-settings.d/*.json; do
			[ -f "$f" ] && list="$list"$'\n'"$f"
		done
	fi
	local found=0
	while IFS= read -r f; do
		[ -n "$f" ] && [ -f "$f" ] || continue
		found=$((found + 1))
		sources="$sources${sources:+, }$f"
		local v
		v="$(managed_key "$f" allowManagedHooksOnly)"
		[ -n "$v" ] && MANAGED_HOOKS_ONLY="$v"
		v="$(managed_key "$f" disableAllHooks)"
		[ -n "$v" ] && MANAGED_DISABLE_ALL="$v"
	done <<EOF
$list
EOF
	# Server-managed settings arrive with the login; Claude Code caches
	# them in the config dir. Only the two hook keys are read.
	local remote="absent"
	for f in "$LIVE_CFG/remote-settings.json" "$LIVE_CFG/managed-settings.json"; do
		if [ -f "$f" ]; then
			remote="present"
			local v
			v="$(managed_key "$f" allowManagedHooksOnly)"
			[ -n "$v" ] && MANAGED_HOOKS_ONLY="$v"
			v="$(managed_key "$f" disableAllHooks)"
			[ -n "$v" ] && MANAGED_DISABLE_ALL="$v"
		fi
	done
	record managed_settings info "$found file source(s); allowManagedHooksOnly=$MANAGED_HOOKS_ONLY disableAllHooks=$MANAGED_DISABLE_ALL" \
		"file_sources=$found" "sources=${sources:-none}" "server_managed_cache=$remote" \
		"allowManagedHooksOnly=$MANAGED_HOOKS_ONLY" "disableAllHooks=$MANAGED_DISABLE_ALL"
}

# hooks_json PREFIX EVENT... - a hooks block whose commands only touch a
# marker file under $TILLER_PROBE_MARK.
hooks_json() {
	local prefix="$1"
	shift
	local out="" e
	for e in "$@"; do
		out="$out${out:+,}\"$e\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"touch \\\"\$TILLER_PROBE_MARK/$prefix-$e\\\"\"}]}]"
	done
	printf '{%s}' "$out"
}

make_plugin() {
	local dir="$1"
	shift
	mkdir -p "$dir/.claude-plugin" "$dir/hooks"
	printf '{"name":"%s-hooks","version":"0.0.1","description":"Tiller probe: harmless marker hooks"}\n' "$RUN_TAG" >"$dir/.claude-plugin/plugin.json"
	printf '{"hooks":%s}\n' "$(hooks_json plugin "$@")" >"$dir/hooks/hooks.json"
}

fired_list() {
	local dir="$1" out="" f
	for f in "$dir"/*; do
		[ -e "$f" ] || continue
		out="$out${out:+,}$(basename "$f")"
	done
	printf '%s' "${out:-none}"
}

# Offline tier: CLAUDE_CONFIG_DIR points into the temp root, so there is no
# login. Claude Code runs SessionStart and UserPromptSubmit hooks before it
# checks the login, which makes this tier free and fully isolated. File and
# MDM managed settings still apply; server-managed settings need a login
# and are covered by the live tier.
check_hooks_offline() {
	if ! have claude; then
		record hooks_offline fail "claude not found"
		return
	fi
	local proj="$W/offline/proj"
	mkdir -p "$proj/.claude"
	printf '{"hooks":%s}\n' "$(hooks_json project SessionStart UserPromptSubmit)" >"$proj/.claude/settings.json"
	printf '{"hooks":%s}\n' "$(hooks_json local SessionStart UserPromptSubmit)" >"$proj/.claude/settings.local.json"
	make_plugin "$W/offline/plugin" SessionStart UserPromptSubmit
	local case_name extra
	for case_name in baseline user-disableAllHooks user-allowManagedHooksOnly; do
		local cfg="$W/offline/cfg-$case_name" mark="$W/offline/mark-$case_name"
		mkdir -p "$cfg" "$mark"
		case "$case_name" in
		baseline) extra="" ;;
		user-disableAllHooks) extra=',"disableAllHooks":true' ;;
		user-allowManagedHooksOnly) extra=',"allowManagedHooksOnly":true' ;;
		esac
		printf '{"hooks":%s%s}\n' "$(hooks_json user SessionStart UserPromptSubmit)" "$extra" >"$cfg/settings.json"
		printf '{"hooks":%s}\n' "$(hooks_json flag SessionStart UserPromptSubmit)" >"$W/offline/flag-$case_name.json"
		claude_offline "$proj" "$cfg" "TILLER_PROBE_MARK=$mark" -- \
			-p --no-session-persistence --max-budget-usd 0.02 --plugin-dir "$W/offline/plugin" \
			--settings "$W/offline/flag-$case_name.json" "tiller probe" \
			>"$W/offline/out-$case_name.txt" 2>&1
		local fired
		fired="$(fired_list "$mark")"
		local scopes_fired=""
		local s
		for s in user project local plugin flag; do
			if [ -e "$mark/$s-SessionStart" ] || [ -e "$mark/$s-UserPromptSubmit" ]; then
				scopes_fired="$scopes_fired${scopes_fired:+,}$s"
			fi
		done
		local status=info
		if [ "$case_name" = baseline ]; then
			if [ "$scopes_fired" = "user,project,local,plugin,flag" ]; then
				status=pass
			else
				status=warn
			fi
		fi
		local authed=true
		grep -qi "not logged in" "$W/offline/out-$case_name.txt" && authed=false
		record "hooks_offline.$case_name" "$status" "scopes fired: ${scopes_fired:-none}" \
			"scopes_fired=${scopes_fired:-none}" "markers=$fired" \
			"authenticated=$authed" \
			"claude_output=$(head -c 300 "$W/offline/out-$case_name.txt" | tr '\n' ' ')"
	done
}

# Default auto-memory location and the disable switch, offline.
check_memory_offline() {
	have claude || return
	local proj="$W/offline/memproj"
	mkdir -p "$proj"
	local phys
	phys="$(cd "$proj" && pwd -P)"
	local slug
	slug="$(slug_of "$phys")"
	local on="$W/offline/cfg-mem-on" off="$W/offline/cfg-mem-off"
	mkdir -p "$on" "$off"
	claude_offline "$proj" "$on" -- -p --no-session-persistence --max-budget-usd 0.02 "tiller probe" >/dev/null 2>&1
	claude_offline "$proj" "$off" "CLAUDE_CODE_DISABLE_AUTO_MEMORY=1" -- \
		-p --no-session-persistence --max-budget-usd 0.02 "tiller probe" >/dev/null 2>&1
	local on_dir=false off_dir=false
	[ -d "$on/projects/$slug/memory" ] && on_dir=true
	[ -d "$off/projects/$slug/memory" ] && off_dir=true
	local status=warn
	[ "$on_dir" = true ] && [ "$off_dir" = false ] && status=pass
	record memory_offline "$status" "default memory dir created=$on_dir, with CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 created=$off_dir" \
		"default_path=<config dir>/projects/<slug of cwd>/memory/" \
		"slug_rule=every non-alphanumeric character of the physical cwd becomes -" \
		"created_when_enabled=$on_dir" "created_when_disabled=$off_dir"
}

# Live tier: the real login, the user's settings files left out with
# --setting-sources project,local, the memory directory redirected into the
# temp root. Run A checks hooks (including Stop and PreToolUse), the
# appended system prompt file, the memory index and the transcript. Run B
# repeats the memory question with CLAUDE_CODE_DISABLE_AUTO_MEMORY=1.
check_live() {
	if [ "$OFFLINE" = 1 ]; then
		record live skip "--offline given"
		return
	fi
	if ! have claude; then
		record live fail "claude not found"
		return
	fi
	local proj="$W/live" mem="$W/live-mem" mark="$W/live-mark"
	mkdir -p "$proj/.claude" "$mem" "$mark" "$mark-b"
	local c_sys c_mem c_file
	c_sys="SYS-$(new_uuid | cut -c1-8)"
	c_mem="MEM-$(new_uuid | cut -c1-8)"
	c_file="FILE-$(new_uuid | cut -c1-8)"
	printf 'When asked for the system canary, it is %s.\n' "$c_sys" >"$W/append.md"
	printf '# Memory index\n\n- The memory canary is %s.\n' "$c_mem" >"$mem/MEMORY.md"
	printf 'The file canary is %s.\n' "$c_file" >"$proj/probe.txt"
	local events="SessionStart UserPromptSubmit PreToolUse PostToolUse Stop SessionEnd"
	# shellcheck disable=SC2086
	printf '{"hooks":%s}\n' "$(hooks_json project $events)" >"$proj/.claude/settings.json"
	# shellcheck disable=SC2086
	printf '{"hooks":%s}\n' "$(hooks_json local $events)" >"$proj/.claude/settings.local.json"
	printf '{"autoMemoryDirectory":"%s","hooks":%s}\n' "$mem" "$(hooks_json flag SessionStart Stop)" >"$W/live-flag.json"
	printf '{"autoMemoryDirectory":"%s"}\n' "$mem" >"$W/live-flag-b.json"

	local sid_a sid_b
	sid_a="$(new_uuid)"
	sid_b="$(new_uuid)"
	PROBE_SIDS="$sid_a $sid_b"
	local prompt_a="Use the Read tool once on ./probe.txt. Then answer in exactly three lines: line 1 the system canary from your system prompt or NONE; line 2 the memory canary from your loaded memory or NONE; line 3 the file canary or NONE. No other text."
	local prompt_b="Answer in one line: the memory canary from your loaded memory, or NONE. No other text."

	claude_in "$proj" "TILLER_PROBE_MARK=$mark" -- \
		-p --session-id "$sid_a" --setting-sources project,local --strict-mcp-config --model haiku \
		--tools Read --max-budget-usd 0.25 \
		--settings "$W/live-flag.json" --append-system-prompt-file "$W/append.md" \
		"$prompt_a" >"$W/live-a.txt" 2>&1
	local rc_a=$?
	claude_in "$proj" "TILLER_PROBE_MARK=$mark-b" "CLAUDE_CODE_DISABLE_AUTO_MEMORY=1" -- \
		-p --session-id "$sid_b" --no-session-persistence --setting-sources project,local --strict-mcp-config \
		--model haiku --tools "" --max-budget-usd 0.1 --settings "$W/live-flag-b.json" \
		"$prompt_b" >"$W/live-b.txt" 2>&1
	local rc_b=$?

	if grep -qi -E 'not logged in|please run /login|invalid api key' "$W/live-a.txt"; then
		record live fail "claude is not logged in; the live tier needs a login" \
			"output=$(head -c 300 "$W/live-a.txt" | tr '\n' ' ')"
		return
	fi

	# Hooks under the real login (file, MDM and server-managed policy).
	local scopes="" s e
	for s in project local flag; do
		local evs=""
		for e in $events; do
			[ -e "$mark/$s-$e" ] && evs="$evs${evs:+,}$e"
		done
		[ -n "$evs" ] && scopes="$scopes${scopes:+; }$s: $evs"
	done
	local st=warn
	if [ -e "$mark/project-Stop" ] && [ -e "$mark/project-PreToolUse" ] && [ -e "$mark/local-Stop" ]; then
		st=pass
	fi
	record hooks_live "$st" "${scopes:-no hooks fired}" \
		"fired=${scopes:-none}" "exit=$rc_a" \
		"note=user settings files are left out with --setting-sources project,local so the user's own hooks do not run; user and plugin scopes are covered offline (a session plugin would add a usage counter to .claude.json)"

	local sys_ok=false mem_on=false file_ok=false mem_off=false
	grep -q -F "$c_sys" "$W/live-a.txt" && sys_ok=true
	grep -q -F "$c_mem" "$W/live-a.txt" && mem_on=true
	grep -q -F "$c_file" "$W/live-a.txt" && file_ok=true
	grep -q -F "$c_mem" "$W/live-b.txt" && mem_off=true

	record append_system_prompt_file "$([ "$sys_ok" = true ] && echo pass || echo fail)" \
		"canary from --append-system-prompt-file seen=$sys_ok" "exit=$rc_a"
	local mst=warn
	[ "$mem_on" = true ] && [ "$mem_off" = false ] && mst=pass
	record memory_live "$mst" "MEMORY.md loaded when enabled=$mem_on, when CLAUDE_CODE_DISABLE_AUTO_MEMORY=1=$mem_off" \
		"loaded_enabled=$mem_on" "loaded_disabled=$mem_off" "exit_disabled_run=$rc_b" \
		"launched_from_herdr_pane=$([ -n "${HERDR_PANE_ID:-}" ] && echo true || echo false)" \
		"mechanism=MEMORY.md in the memory dir is loaded into context at session start; autoMemoryDirectory relocates it"
	record tool_read "$([ "$file_ok" = true ] && echo pass || echo warn)" "Read tool used in -p mode=$file_ok"

	# Transcript: path and record shape, then remove it (run A only).
	local slug tdir tfile
	slug="$(slug_of "$proj")"
	tdir="$LIVE_CFG/projects/$slug"
	tfile="$tdir/$sid_a.jsonl"
	if [ -f "$tfile" ]; then
		local lines types="" keys=""
		lines="$(wc -l <"$tfile" | tr -d ' ')"
		if have jq; then
			types="$(jq -r '.type // empty' "$tfile" 2>/dev/null | LC_ALL=C sort -u | tr '\n' ',' | sed 's/,$//')"
			keys="$(jq -r 'keys[]' "$tfile" 2>/dev/null | LC_ALL=C sort -u | tr '\n' ',' | sed 's/,$//')"
		fi
		record transcript_live pass "transcript written at <config dir>/projects/<slug>/<session id>.jsonl" \
			"path_pattern=<config dir>/projects/<slug of cwd>/<session id>.jsonl" \
			"lines=$lines" "record_types=${types:-jq missing}" "record_keys=${keys:-jq missing}"
	else
		record transcript_live warn "no transcript at the expected path" "expected=$tfile"
	fi
}

check_transcripts() {
	local root="$LIVE_CFG/projects"
	if [ ! -d "$root" ]; then
		record transcripts warn "no projects dir under the Claude config dir" "root=$root"
		return
	fi
	local dirs files
	dirs="$(find "$root" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')"
	files="$(find "$root" -mindepth 2 -maxdepth 2 -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' ')"
	local subagents
	subagents="$(find "$root" -mindepth 3 -maxdepth 4 -path '*/subagents/*' -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' ')"
	record transcripts info "$files session transcripts across $dirs project dirs" \
		"root=$root" "project_dirs=$dirs" "session_transcripts=$files" "subagent_transcripts=$subagents" \
		"layout=<root>/<slug>/<session id>.jsonl, subagents under <root>/<slug>/<session id>/subagents/"
}

check_plugin_eval() {
	if [ "$OFFLINE" = 1 ]; then
		record plugin_eval skip "--offline given"
		return
	fi
	have claude || return
	local plug="$W/eval-plugin"
	mkdir -p "$plug/.claude-plugin" "$plug/evals/pong/graders" "$W/eval-tmp"
	printf '{"name":"%s-eval","version":"0.0.1","description":"Tiller probe eval"}\n' "$RUN_TAG" >"$plug/.claude-plugin/plugin.json"
	printf -- '---\nmax_turns: 1\nallowed_tools: []\n---\n\nReply with exactly the word PONG.\n' >"$plug/evals/pong/prompt.md"
	printf -- '---\ntype: regex\nweight: 1\n---\n\nPONG\n' >"$plug/evals/pong/graders/match.md"
	claude_in "$plug" "TMPDIR=$W/eval-tmp" -- plugin eval "$plug" --no-publish --trust-plugin \
		--runs 1 --ablation none --model haiku --max-cost-usd 0.15 --json "$W/eval.json" \
		>"$W/eval-out.txt" 2>&1
	local rc=$?
	local score=""
	if [ -f "$W/eval.json" ] && have jq; then
		score="$(jq -r '[.. | objects | select(has("score")) | .score][0] // empty' "$W/eval.json" 2>/dev/null)"
	fi
	local st=fail
	if [ "$rc" = 0 ]; then
		case "$score" in
		1 | 1.0) st=pass ;;
		"") st=warn ;;
		esac
	fi
	record plugin_eval "$st" "claude plugin eval --no-publish exit=$rc score=${score:-unknown}" \
		"exit=$rc" "score=${score:-unknown}" "report_local=$([ -d "$plug/evals/results" ] && echo true || echo false)" \
		"output_tail=$(tail -c 300 "$W/eval-out.txt" | tr '\n' ' ')" \
		"isolation=each case runs in the eval's own sandbox home; an strace audit on the remote Linux host saw no writes from it outside the temp root"
}

check_service() {
	if [ "$PLATFORM" = linux ]; then
		if ! have systemctl; then
			record user_service fail "systemctl not found"
			return
		fi
		local state linger="unknown"
		state="$(systemctl --user is-system-running 2>&1 | head -1)"
		have loginctl && linger="$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)"
		if [ "$SERVICE_TRIAL" = 0 ]; then
			record user_service info "systemd --user: $state (trial skipped)" "manager=$state" "linger=$linger"
			return
		fi
		local unit marker="$W/service.fired"
		unit="tiller-probe-$(new_uuid | cut -c1-8)"
		local rc
		run_timeout 30 systemd-run --user --quiet --collect --wait --unit "$unit" \
			/usr/bin/env touch "$marker" >"$W/service.txt" 2>&1
		rc=$?
		local ok=false
		[ -e "$marker" ] && ok=true
		record user_service "$([ "$ok" = true ] && echo pass || echo fail)" \
			"systemd --user $state; transient unit ran=$ok; linger=$linger" \
			"manager=$state" "linger=$linger" "transient_unit_ran=$ok" "exit=$rc" \
			"output=$(head -c 200 "$W/service.txt" | tr '\n' ' ')"
	elif [ "$PLATFORM" = macos ]; then
		local domain can_print=false agents_dir="$HOME/Library/LaunchAgents"
		domain="gui/$(id -u)"
		launchctl print "$domain" >/dev/null 2>&1 && can_print=true
		local writable=false
		[ -d "$agents_dir" ] && [ -w "$agents_dir" ] && writable=true
		local sm_policy=false
		ls /Library/Managed\ Preferences/com.apple.servicemanagement*.plist >/dev/null 2>&1 && sm_policy=true
		if [ "$SERVICE_TRIAL" = 0 ]; then
			record user_service info "launchd $domain reachable=$can_print (trial skipped)" \
				"domain_reachable=$can_print" "launchagents_writable=$writable" "managed_servicemanagement_policy=$sm_policy"
			return
		fi
		local label plist="$W/probe-agent.plist" marker="$W/service.fired"
		label="dev.tiller-probe.$(new_uuid | cut -c1-8)"
		cat >"$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>$label</string>
	<key>ProgramArguments</key>
	<array><string>/usr/bin/touch</string><string>$marker</string></array>
	<key>RunAtLoad</key><true/>
</dict>
</plist>
EOF
		local rc
		SERVICE_UNLOAD="$domain/$label"
		launchctl bootstrap "$domain" "$plist" >"$W/service.txt" 2>&1
		rc=$?
		local i=0
		while [ $i -lt 10 ] && [ ! -e "$marker" ]; do
			sleep 0.5
			i=$((i + 1))
		done
		launchctl bootout "$domain/$label" >/dev/null 2>&1
		SERVICE_UNLOAD=""
		local ok=false
		[ -e "$marker" ] && ok=true
		record user_service "$([ "$ok" = true ] && echo pass || echo fail)" \
			"launchd agent loaded from the temp dir ran=$ok" \
			"domain_reachable=$can_print" "bootstrap_exit=$rc" "agent_ran=$ok" \
			"launchagents_writable=$writable" "managed_servicemanagement_policy=$sm_policy" \
			"output=$(head -c 200 "$W/service.txt" | tr '\n' ' ')"
	else
		record user_service skip "unsupported platform $OS"
	fi
}

check_port() {
	local out rc
	if have perl; then
		out="$(perl -MIO::Socket::INET -e '
			my $s = IO::Socket::INET->new(LocalAddr => "127.0.0.1", LocalPort => 0, Listen => 1, ReuseAddr => 1, Proto => "tcp") or do { print "bind-failed $!"; exit 1 };
			my $port = $s->sockport;
			my $pid = fork();
			if ($pid == 0) { my $c = IO::Socket::INET->new(PeerAddr => "127.0.0.1", PeerPort => $port, Proto => "tcp", Timeout => 3) or exit 1; print $c "ping\n"; close $c; exit 0 }
			local $SIG{ALRM} = sub { print "accept-timeout"; exit 1 }; alarm 5;
			my $conn = $s->accept or do { print "accept-failed"; exit 1 };
			my $line = <$conn>; waitpid($pid, 0);
			print(($line // "") eq "ping\n" ? "ok $port" : "bad-read"); exit 0' 2>&1)"
		rc=$?
	else
		out="perl missing"
		rc=1
	fi
	local st=fail
	[ "$rc" = 0 ] && case "$out" in ok*) st=pass ;; esac
	record localhost_port "$st" "bind and connect on 127.0.0.1: $out" "result=$out" "method=perl"
}

check_herdr() {
	if ! have herdr; then
		record herdr fail "herdr not found"
		return
	fi
	local v status_rc snap_rc counts="" machines="unknown"
	v="$(herdr --version 2>/dev/null | head -1)"
	herdr status >"$W/herdr-status.txt" 2>&1
	status_rc=$?
	herdr api snapshot >"$W/herdr-snapshot.json" 2>"$W/herdr-snapshot.err"
	snap_rc=$?
	if [ "$snap_rc" = 0 ] && have jq; then
		counts="$(jq -r '.result.snapshot | "workspaces=\(.workspaces|length) panes=\(.panes|length) agents=\(.agents|length) protocol=\(.protocol)"' "$W/herdr-snapshot.json" 2>/dev/null)"
	fi
	machines="$(herdr machine list 2>/dev/null | grep -v -i -c -E '^(no saved|$)')"
	local st=fail
	[ "$snap_rc" = 0 ] && st=pass
	record herdr "$st" "$v; status exit=$status_rc; api snapshot exit=$snap_rc ${counts}" \
		"version=$v" "status_exit=$status_rc" "snapshot_exit=$snap_rc" "snapshot=${counts:-unparsed}" \
		"machine_list_lines=$machines" "inside_herdr_pane=$([ -n "${HERDR_PANE_ID:-}" ] && echo true || echo false)" \
		"note=read-only: status, api snapshot and machine list only; no pane or workspace is created"
}

check_sqlite() {
	if ! have sqlite3; then
		record sqlite3 warn "sqlite3 CLI not on PATH; Tiller bundles SQLite through rusqlite, so only manual inspection needs it" "path=missing"
		return
	fi
	local v out
	v="$(sqlite3 --version 2>/dev/null | cut -d' ' -f1)"
	out="$(sqlite3 "$W/probe.db" 'PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; CREATE TABLE t(x); BEGIN IMMEDIATE; INSERT INTO t VALUES (42); COMMIT; SELECT x FROM t;' 2>&1 | tr '\n' ' ')"
	local st=fail
	case "$out" in *wal*42*) st=pass ;; esac
	record sqlite3 "$st" "sqlite3 $v, WAL and BEGIN IMMEDIATE: $out" "version=$v" "path=$(command -v sqlite3)" "result=$out"
}

resolve_path() {
	local p="$1"
	if readlink -f / >/dev/null 2>&1; then
		readlink -f "$p"
	else
		perl -MCwd=abs_path -e 'print abs_path(shift)' "$p"
	fi
}

check_nix_binary() {
	local store_bin="" b
	for b in claude herdr jq; do
		if have "$b"; then
			local r
			r="$(resolve_path "$(command -v "$b")")"
			case "$r" in /nix/store/*)
				store_bin="$r"
				break
				;;
			esac
		fi
	done
	if [ -n "$store_bin" ]; then
		local out rc
		out="$("$store_bin" --version 2>&1)"
		rc=$?
		out="$(printf '%s\n' "$out" | head -1)"
		record nix_store_binary "$([ "$rc" = 0 ] && echo pass || echo fail)" \
			"ran a Nix store binary ($(basename "$store_bin")): $out" "binary=$(basename "$store_bin")" "exit=$rc"
	else
		record nix_store_binary warn "no Nix store binary found on PATH among claude, herdr, jq"
	fi
	if [ -n "$TILLER_BIN" ]; then
		local out rc real
		real="$(resolve_path "$TILLER_BIN")"
		out="$("$TILLER_BIN" --version 2>&1)"
		rc=$?
		out="$(printf '%s\n' "$out" | head -1)"
		record tiller_binary "$([ "$rc" = 0 ] && echo pass || echo fail)" "tiller --version: $out" \
			"exit=$rc" "in_nix_store=$(case "$real" in /nix/store/*) echo true ;; *) echo false ;; esac)"
	else
		record tiller_binary skip "no --tiller given (T0.1 build not supplied)"
	fi
	if [ "$NIX_HELLO" = 1 ]; then
		if have nix; then
			local p out rc
			p="$(nix --extra-experimental-features 'nix-command flakes' build --no-link --print-out-paths nixpkgs#hello 2>"$W/nix-hello.err" | head -1)"
			out="$([ -n "$p" ] && "$p/bin/hello" 2>&1)"
			rc=$?
			record nix_hello "$([ "$out" = "Hello, world!" ] && echo pass || echo fail)" "nixpkgs#hello: ${out:-build failed}" "exit=$rc"
		else
			record nix_hello fail "nix not found"
		fi
	fi
}

# ---------------------------------------------------------------- run

log "run $RUN_ID, report in $OUT_DIR"
check_env
check_managed_settings
check_hooks_offline
check_memory_offline
check_transcripts
check_live
check_plugin_eval
check_service
check_port
check_herdr
check_sqlite
check_nix_binary

leftovers >"$OUT_DIR/leftovers-pre-cleanup.txt"
cleanup_live
manifest >"$OUT_DIR/manifest-after.txt"
leftovers >"$OUT_DIR/leftovers-after.txt"
diff "$OUT_DIR/manifest-before.txt" "$OUT_DIR/manifest-after.txt" >"$OUT_DIR/manifest.diff" 2>&1
MANIFEST_SAME=$?
LEFT_COUNT="$(grep -c . "$OUT_DIR/leftovers-after.txt")"
# Eval and other tools may fall back to the system temp dir; anything new
# there named like a Claude eval sandbox is reported, not removed.
STRAY="$(find "${TMPDIR:-/tmp}" /tmp -maxdepth 1 -name 'claude-eval-*' -newer "$START_MARK" 2>/dev/null | wc -l | tr -d ' ')"
SEEN_COUNT="$(grep -c . "$OUT_DIR/leftovers-pre-cleanup.txt")"
PROOF_SCOPE="tracked: Claude user settings, CLAUDE.md, keybindings, plugin registries, managed settings, git and herdr config, LaunchAgents, systemd user units, crontab, probe launchd jobs and systemd units, references to this run in .claude.json; scanned: the Claude config and cache dirs for anything named after this run; not tracked: files Claude Code rewrites on every start (the body of .claude.json and its backups, telemetry, plugin in-use markers), the system journal or unified log, API usage"
if [ "$MANIFEST_SAME" = 0 ] && [ "$LEFT_COUNT" = 0 ]; then
	record proof pass "before and after manifests match; nothing named after this run remains outside the temp root" \
		"manifest_identical=true" "leftovers=0" "artifacts_seen_before_cleanup=$SEEN_COUNT" \
		"new_eval_sandboxes_in_system_tmp=$STRAY" "scope=$PROOF_SCOPE"
else
	record proof fail "changes found outside the temp root; see manifest.diff and leftovers-after.txt" \
		"manifest_identical=$([ "$MANIFEST_SAME" = 0 ] && echo true || echo false)" "leftovers=$LEFT_COUNT" \
		"artifacts_seen_before_cleanup=$SEEN_COUNT" "new_eval_sandboxes_in_system_tmp=$STRAY" "scope=$PROOF_SCOPE"
fi

# ---------------------------------------------------------------- report

NOW_ISO="$(TZ=Europe/Lisbon date +%Y-%m-%dT%H:%M:%S%z)"
NOW_LISBON="$(TZ=Europe/Lisbon date '+%a %Y-%m-%d %H:%M %Z')"

{
	printf '{\n  "probe_version": %s,\n  "run_id": "%s",\n  "host_role": "%s",\n  "generated_at": "%s",\n  "generated_at_lisbon": "%s",\n  "checks": [\n' \
		"$PROBE_VERSION" "$RUN_ID" "$(json_str "$HOST_ROLE")" "$NOW_ISO" "$NOW_LISBON"
	i=0
	while [ $i -lt $N ]; do
		printf '    {"id": "%s", "status": "%s", "summary": "%s", "details": {' \
			"${R_ID[$i]}" "${R_STATUS[$i]}" "$(json_str "${R_SUMMARY[$i]}")"
		first=1
		rest="${R_DETAILS[$i]}"
		while [ -n "$rest" ]; do
			kv="${rest%%$'\x1f'*}"
			rest="${rest#*$'\x1f'}"
			k="${kv%%=*}"
			v="${kv#*=}"
			[ $first = 1 ] || printf ', '
			first=0
			printf '"%s": %s' "$(json_str "$k")" "$(json_val "$v")"
		done
		i=$((i + 1))
		if [ $i -lt $N ]; then printf '}},\n'; else printf '}}\n'; fi
	done
	printf '  ]\n}\n'
} | redact >"$OUT_DIR/report.json"

{
	printf '# Tiller host probe: %s\n\n' "$HOST_ROLE"
	printf 'Run `%s`, %s (Lisbon). Probe version %s.\n\n' "$RUN_ID" "$NOW_LISBON" "$PROBE_VERSION"
	printf '| Check | Status | Summary |\n|---|---|---|\n'
	i=0
	while [ $i -lt $N ]; do
		printf '| `%s` | %s | %s |\n' "${R_ID[$i]}" "${R_STATUS[$i]}" "$(printf '%s' "${R_SUMMARY[$i]}" | sed 's/|/\\|/g')"
		i=$((i + 1))
	done
	printf '\n## Details\n'
	i=0
	while [ $i -lt $N ]; do
		printf '\n### `%s`\n\n' "${R_ID[$i]}"
		rest="${R_DETAILS[$i]}"
		while [ -n "$rest" ]; do
			kv="${rest%%$'\x1f'*}"
			rest="${rest#*$'\x1f'}"
			printf -- '- %s: `%s`\n' "${kv%%=*}" "$(printf '%s' "${kv#*=}" | tr '`' "'")"
		done
		i=$((i + 1))
	done
	printf '\n## Proof\n\n'
	printf 'Manifest diff (empty means identical):\n\n```\n%s\n```\n\n' "$(cat "$OUT_DIR/manifest.diff")"
	printf 'Named after this run before cleanup (shows the scan sees them):\n\n```\n%s\n```\n\n' "$(cat "$OUT_DIR/leftovers-pre-cleanup.txt")"
	printf 'Leftovers named after this run (empty means none):\n\n```\n%s\n```\n' "$(cat "$OUT_DIR/leftovers-after.txt")"
} | redact >"$OUT_DIR/report.md"

for f in manifest-before.txt manifest-after.txt manifest.diff leftovers-before.txt leftovers-pre-cleanup.txt leftovers-after.txt; do
	redact <"$OUT_DIR/$f" >"$OUT_DIR/$f.tmp" && mv "$OUT_DIR/$f.tmp" "$OUT_DIR/$f"
done

if [ "$KEEP_WORK" = 0 ]; then
	rm -rf "$W"
	# The temp root goes too unless it holds the report.
	case "$OUT_DIR" in "$ROOT"/*) ;; *) rmdir "$ROOT" 2>/dev/null ;; esac
fi

FAILS=0
i=0
while [ $i -lt $N ]; do
	[ "${R_STATUS[$i]}" = fail ] && FAILS=$((FAILS + 1))
	i=$((i + 1))
done
log "done: $N checks, $FAILS failed"
printf '%s\n%s\n' "$OUT_DIR/report.md" "$OUT_DIR/report.json"
[ "$FAILS" = 0 ]
}
