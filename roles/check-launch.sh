#!/usr/bin/env bash
# check-launch.sh - launch each fleet role through its real launch form and
# verify its effective instructions, tools, model and memory at startup and
# on resume (T4.1 acceptance; plan v2 points 30 and 31).
#
# For every role it:
#   1. renders the role body (the file without its frontmatter) and the
#      settings fragment with the placeholders filled for a throwaway
#      project, the way `tiller spawn` will;
#   2. plants canaries that must NOT load: a project CLAUDE.md, a rules file
#      and an auto-memory MEMORY.md for that project;
#   3. starts `claude -p` with the launch form from authority.md section 7
#      and asks five scripted questions; the answers and the JSON result
#      (model, permission denials) are the evidence;
#   4. resumes the same session with `--resume` and asks again;
#   5. records which hook events the fragment's hooks fired, through a stub
#      `tiller` that logs its arguments.
#
# The stub stands in for the packaged `tiller` binary only as the hook
# command; it allows every tool call. The guards themselves are T4.3's.
#
# Everything it creates lives under one temp root, except what Claude Code
# writes for the session into its own config dir (the transcript and the
# auto-memory directory for the temp project), which it removes at the end.
# The live calls use the real login and cost a few cents per role.
#
# Usage: roles/check-launch.sh [--role NAME]... [--out DIR] [--model M]
#                              [--no-sandbox] [--keep] [--max-budget-usd N]
#   --role NAME    check one role (repeatable); default: all five
#   --out DIR      write report.md, report.json and raw outputs there
#                  (default: a new directory under $TMPDIR)
#   --model M      override every role's model (for a cheap dry run)
#   --no-sandbox   set sandbox.enabled=false in the fragment, for a host
#                  whose sandbox cannot start; the report says so
#   --keep         keep the temp root for debugging
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd -P)
roles_dir=$here
roles=()
out=""
model_override=""
sandbox=1
keep=0
budget="1.00"

while [ $# -gt 0 ]; do
	case "$1" in
	--role) roles+=("$2"); shift 2 ;;
	--out) out="$2"; shift 2 ;;
	--model) model_override="$2"; shift 2 ;;
	--no-sandbox) sandbox=0; shift ;;
	--keep) keep=1; shift ;;
	--max-budget-usd) budget="$2"; shift 2 ;;
	-h | --help) sed -n '2,30p' "$0"; exit 0 ;;
	*) echo "check-launch: unknown option: $1" >&2; exit 2 ;;
	esac
done
[ ${#roles[@]} -gt 0 ] || roles=(orchestrator scout builder reviewer verifier)

for tool in claude jq git; do
	command -v "$tool" >/dev/null || { echo "check-launch: $tool not found" >&2; exit 2; }
done

root=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/tiller-check-launch.XXXXXX")" && pwd -P)
[ -n "$out" ] || out="$root/out"
mkdir -p "$out"
config_dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"

cleanup() {
	# Claude Code keys its per-project files by the working directory, so the
	# temp project's slug dir holds only this run's transcripts and memory.
	for f in "$root"/*/worktree-path; do
		[ -f "$f" ] || continue
		slug=$(sed 's/[^A-Za-z0-9]/-/g' "$f")
		[ -n "$slug" ] && rm -rf "${config_dir:?}/projects/$slug"
	done
	if [ "$keep" = 1 ]; then
		echo "check-launch: kept $root"
	else
		rm -rf "$root"
	fi
}
trap cleanup EXIT

frontmatter() { # frontmatter FILE KEY
	awk -v key="$2" 'NR==1 && $0!="---" {exit} NR>1 && $0=="---" {exit} NR>1 {i=index($0,":"); if (substr($0,1,i-1)==key) {print substr($0,i+1)}}' "$1" | sed 's/^ *//'
}

body() { # body FILE > stdout: the role file without its frontmatter
	awk 'NR==1 && $0=="---" {skip=1; next} skip && $0=="---" {skip=0; blank=1; next} skip {next} blank && $0=="" {next} {blank=0; print}' "$1"
}

new_uuid() {
	if [ -r /proc/sys/kernel/random/uuid ]; then
		cat /proc/sys/kernel/random/uuid
	else
		uuidgen | tr '[:upper:]' '[:lower:]'
	fi
}

rand() { new_uuid | cut -c1-8; }

prompt_a='This is a launch check. Answer in exactly five lines, each starting with its label, and make no tool call except where a line asks for one.
HEADING: the exact text of the first markdown heading in the additional instructions appended after the default system prompt, or NONE.
MEMORY: every token of the form PROJECT-MEMORY-xxxx, RULES-MEMORY-xxxx or AUTO-MEMORY-xxxx that appears anywhere in your context, space-separated, or NONE.
TOOLS: the names of every tool you can call, comma-separated, sorted.
WRITE: call the Write tool once to create probe-write.txt in the current directory with the content x. Then answer OK if it succeeded, DENIED if the call was refused, or NOTOOL if you have no Write tool.
GUARD: call the Bash tool once with exactly the command curl --version. Then answer OK if it ran, DENIED if the harness refused the call, or NOTOOL if you have no Bash tool.'

prompt_b='This is the second half of the launch check, after a resume. Answer in exactly four lines, each starting with its label, and make no tool call except where a line asks for one.
HEADING: the exact text of the first markdown heading in the additional instructions appended after the default system prompt, or NONE.
MEMORY: every token of the form PROJECT-MEMORY-xxxx, RULES-MEMORY-xxxx or AUTO-MEMORY-xxxx that appears anywhere in your context, space-separated, or NONE.
RESUMED: the label of the first line of my previous message in this conversation, or NONE if this is the first message.
GUARD: call the Bash tool once with exactly the command curl --version. Then answer OK if it ran, DENIED if the harness refused the call, or NOTOOL if you have no Bash tool.'

answer() { # answer RESULT_JSON LABEL -> the text after "LABEL:" or MISSING
	jq -r '.result // ""' "$1" | sed -n "s/^$2: *//p" | head -n 1 | sed 's/^$/EMPTY/' | grep . || echo MISSING
}

word() { # word RESULT_JSON LABEL -> the first word of the answer, upper case, no punctuation
	answer "$1" "$2" | awk '{print toupper($1)}' | tr -d ':.,;-'
}

model_used() { # the model ids the result charged, comma-separated
	jq -r '(.modelUsage // {}) | keys | join(",")' "$1"
}

denials() { # permission denials recorded by Claude Code, as "Tool" names
	jq -r '[(.permission_denials // [])[] | .tool_name] | unique | join(",")' "$1"
}

verdicts=()
summary_rows=()

check_role() {
	local role=$1
	local file="$roles_dir/$role.md"
	local template="$roles_dir/$role.settings.json"
	if [ ! -f "$file" ] || [ ! -f "$template" ]; then
		echo "check-launch: no such role: $role" >&2
		verdicts+=("$role: FAIL no such role")
		summary_rows+=("| $role | | no such role | | | | | | | | FAIL |")
		return 0
	fi

	local w="$root/$role"
	mkdir -p "$w/stub" "$w/scratch" "$w/home" "$w/pool" "$w/checkouts"
	local project="$w/project"
	local worktree="$project"
	if [ "$role" = orchestrator ]; then
		# The orchestrator works in the project checkout, which it may read
		# but never edit.
		worktree="$w/checkouts/alpha"
		mkdir -p "$worktree"
		ln -s "$worktree" "$project"
	else
		mkdir -p "$project"
	fi

	local model effort mode
	model=$(frontmatter "$file" model)
	effort=$(frontmatter "$file" effort)
	mode=$(frontmatter "$file" permissionMode)
	[ -n "$model_override" ] && model="$model_override"

	# 1. The stub tiller: logs every hook invocation, allows everything.
	cat >"$w/stub/tiller" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"${TILLER_CHECK_LOG:-/dev/null}"
cat >/dev/null
exit 0
EOF
	chmod +x "$w/stub/tiller"

	# 2. Canaries that must not load.
	local c_project c_rules c_auto
	c_project="PROJECT-MEMORY-$(rand)"
	c_rules="RULES-MEMORY-$(rand)"
	c_auto="AUTO-MEMORY-$(rand)"
	git -C "$worktree" init -q -b main
	printf '# Project memory\n\nThe project canary is %s.\n' "$c_project" >"$worktree/CLAUDE.md"
	mkdir -p "$worktree/.claude/rules"
	printf -- '---\npaths:\n  - "**/*"\n---\nThe rules canary is %s.\n' "$c_rules" >"$worktree/.claude/rules/canary.md"
	git -C "$worktree" add -A && git -C "$worktree" -c user.name=check -c user.email=check@example.com commit -q -m "canaries"
	local slug memdir
	slug=$(printf '%s' "$worktree" | sed 's/[^A-Za-z0-9]/-/g')
	memdir="$config_dir/projects/$slug/memory"
	mkdir -p "$memdir"
	printf '# Memory index\n\n- The auto memory canary is %s.\n' "$c_auto" >"$memdir/MEMORY.md"

	# 3. Render the role body and the fragment the way spawn will.
	body "$file" >"$w/role.md"
	jq --arg tiller "$w/stub/tiller" --arg worktree "$worktree" --arg scratch "$w/scratch" \
		--arg home "$w/home" --arg pool "$w/pool" --arg checkouts "$w/checkouts" --arg default main \
		--argjson sandbox "$([ "$sandbox" = 1 ] && echo true || echo false)" '
		def fill: gsub("<tiller>"; $tiller) | gsub("<worktree>"; $worktree) | gsub("<scratch>"; $scratch)
			| gsub("<home>"; $home) | gsub("<pool>"; $pool) | gsub("<checkouts>"; $checkouts) | gsub("<default>"; $default);
		walk(if type == "string" then fill else . end)
		| walk(if type == "array" then map(select(. != "<network_allow>" and . != "<no_mistakes_socket>")) else . end)
		| .sandbox.enabled = $sandbox
		| .sandbox.failIfUnavailable = $sandbox
	' "$template" >"$w/settings.json"
	if grep -q '<[a-z_]*>' "$w/settings.json"; then
		echo "check-launch: $role: placeholder left in the rendered fragment" >&2
		grep -n '<[a-z_]*>' "$w/settings.json" >&2
		verdicts+=("$role: FAIL placeholder left in the rendered fragment")
		summary_rows+=("| $role | $model | fragment not rendered | | | | | | | | FAIL |")
		return 0
	fi

	local sid
	sid=$(new_uuid)
	: >"$w/hooks-a.log"
	: >"$w/hooks-b.log"
	printf '%s\n' "$worktree" >"$w/worktree-path"
	local launch=(claude -p --output-format json
		--model "$model" --effort "$effort"
		--append-system-prompt-file "$w/role.md" --settings "$w/settings.json"
		--permission-mode "$mode" --strict-mcp-config --mcp-config "$roles_dir/mcp.json"
		--max-turns 8 --max-budget-usd "$budget")
	local envs=(CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 TILLER_SESSION_ID="check-$role" TILLER_RUN_ID="check-$role" TILLER_ROLE="$role")

	echo "check-launch: $role: startup ($model, $effort, $mode)"
	local rc_a=0
	(cd "$worktree" && env "${envs[@]}" TILLER_CHECK_LOG="$w/hooks-a.log" timeout -k 15 420 "${launch[@]}" --session-id "$sid" "$prompt_a") >"$w/a.json" 2>"$w/a.err" || rc_a=$?
	cp "$w/a.json" "$out/$role-startup.json" 2>/dev/null || true
	cp "$w/a.err" "$out/$role-startup.err" 2>/dev/null || true
	if [ "$rc_a" != 0 ] || ! jq -e '.result' "$w/a.json" >/dev/null 2>&1; then
		local why
		why=$(head -c 300 "$w/a.err" | tr '\n' ' ')
		verdicts+=("$role: FAIL startup launch exit $rc_a: $why")
		summary_rows+=("| $role | $model | launch failed (exit $rc_a) | | | | | | | | FAIL |")
		jq -n --arg role "$role" --arg model "$model" --arg why "$why" --arg rc "$rc_a" \
			'{role: $role, launch: {model: $model}, failures: ["startup launch exit \($rc): \($why)"], verdict: "fail"}' \
			>"$out/$role.summary.json"
		return 0
	fi

	echo "check-launch: $role: resume"
	local rc_b=0
	(cd "$worktree" && env "${envs[@]}" TILLER_CHECK_LOG="$w/hooks-b.log" timeout -k 15 420 "${launch[@]}" --resume "$sid" "$prompt_b") >"$w/b.json" 2>"$w/b.err" || rc_b=$?
	cp "$w/b.json" "$out/$role-resume.json" 2>/dev/null || true
	cp "$w/b.err" "$out/$role-resume.err" 2>/dev/null || true

	# 4. Read the evidence.
	local heading_a memory_a tools_a write_a guard_a model_a denials_a
	heading_a=$(answer "$w/a.json" HEADING)
	memory_a=$(answer "$w/a.json" MEMORY)
	tools_a=$(answer "$w/a.json" TOOLS)
	write_a=$(word "$w/a.json" WRITE)
	guard_a=$(word "$w/a.json" GUARD)
	model_a=$(model_used "$w/a.json")
	denials_a=$(denials "$w/a.json")
	local heading_b="launch failed" memory_b="" resumed_b="" guard_b="" model_b="" sid_b=""
	if [ "$rc_b" = 0 ] && jq -e '.result' "$w/b.json" >/dev/null 2>&1; then
		heading_b=$(answer "$w/b.json" HEADING)
		memory_b=$(answer "$w/b.json" MEMORY)
		resumed_b=$(word "$w/b.json" RESUMED)
		guard_b=$(word "$w/b.json" GUARD)
		model_b=$(model_used "$w/b.json")
		sid_b=$(jq -r '.session_id // ""' "$w/b.json")
	fi
	local hooks_a hooks_b
	hooks_a=$(sed -n 's/^hook //p; s/^guard pretool.*/PreToolUse(guard)/p' "$w/hooks-a.log" 2>/dev/null | sort -u | tr '\n' ' ' | sed 's/ $//')
	hooks_b=$(sed -n 's/^hook //p; s/^guard pretool.*/PreToolUse(guard)/p' "$w/hooks-b.log" 2>/dev/null | sort -u | tr '\n' ' ' | sed 's/ $//')
	local written=no
	[ -e "$worktree/probe-write.txt" ] && written=yes
	local memdir_after=present
	[ -d "$memdir" ] || memdir_after=removed

	# 5. Expectations.
	local title
	title=$(sed -n 's/^# //p' "$w/role.md" | head -n 1)
	local fails=()
	[ "$heading_a" = "# $title" ] || [ "$heading_a" = "$title" ] || fails+=("startup heading '$heading_a' is not '# $title'")
	[ "$memory_a" = NONE ] || fails+=("startup memory leaked: $memory_a")
	[ "$guard_a" = DENIED ] || fails+=("startup curl was not denied: $guard_a")
	case "$model" in
	*fable*) case "$model_a" in *fable*) ;; *) fails+=("startup model $model_a is not fable") ;; esac ;;
	*opus*) case "$model_a" in *opus*) ;; *) fails+=("startup model $model_a is not opus") ;; esac ;;
	*) case "$model_a" in *"$model"*) ;; *) fails+=("startup model $model_a is not $model") ;; esac ;;
	esac
	case "$role" in
	reviewer)
		[ "$write_a" = NOTOOL ] || fails+=("reviewer still has a Write tool: $write_a")
		[ "$written" = no ] || fails+=("reviewer wrote a file")
		case ",$tools_a," in *,Edit,* | *,Write,* | *,NotebookEdit,*) fails+=("reviewer lists an edit tool: $tools_a") ;; esac
		;;
	orchestrator)
		[ "$written" = no ] || fails+=("orchestrator wrote into the checkout")
		case ",$tools_a," in *,Agent,*) fails+=("orchestrator lists Agent: $tools_a") ;; esac
		;;
	*)
		[ "$write_a" = OK ] && [ "$written" = yes ] || fails+=("$role could not write in its worktree: $write_a (file: $written)")
		;;
	esac
	[ "$heading_b" = "# $title" ] || [ "$heading_b" = "$title" ] || fails+=("resume heading '$heading_b' is not '# $title'")
	[ "$memory_b" = NONE ] || fails+=("resume memory leaked: $memory_b")
	[ "$resumed_b" = HEADING ] || fails+=("resume did not continue the session: RESUMED=$resumed_b")
	[ "$sid_b" = "$sid" ] || fails+=("resume session id $sid_b differs from $sid")
	[ "$guard_b" = DENIED ] || fails+=("resume curl was not denied: $guard_b")
	case "$hooks_a" in *SessionStart*) ;; *) fails+=("SessionStart hook from the fragment did not fire: '$hooks_a'") ;; esac

	jq -n --arg role "$role" --arg model "$model" --arg effort "$effort" --arg mode "$mode" --arg sandbox "$sandbox" \
		--arg heading_a "$heading_a" --arg memory_a "$memory_a" --arg tools_a "$tools_a" --arg write_a "$write_a" \
		--arg guard_a "$guard_a" --arg model_a "$model_a" --arg denials_a "$denials_a" --arg written "$written" \
		--arg heading_b "$heading_b" --arg memory_b "$memory_b" --arg resumed_b "$resumed_b" --arg guard_b "$guard_b" \
		--arg model_b "$model_b" --arg session "$sid" --arg session_b "$sid_b" --arg hooks_a "$hooks_a" --arg hooks_b "$hooks_b" \
		--arg memdir "$memdir_after" --argjson fails "$(printf '%s\n' "${fails[@]:-}" | jq -R . | jq -s 'map(select(. != ""))')" \
		'{role: $role, launch: {model: $model, effort: $effort, permission_mode: $mode, sandbox: ($sandbox == "1")},
		  startup: {heading: $heading_a, memory: $memory_a, tools: $tools_a, write: $write_a, file_written: $written,
		            curl_probe: $guard_a, model_used: $model_a, permission_denials: $denials_a, hooks_fired: $hooks_a},
		  resume: {heading: $heading_b, memory: $memory_b, resumed: $resumed_b, curl_probe: $guard_b, model_used: $model_b,
		           session_id_kept: ($session == $session_b), hooks_fired: $hooks_b},
		  auto_memory_dir_after: $memdir, failures: $fails, verdict: (if ($fails | length) == 0 then "pass" else "fail" end)}' \
		>"$out/$role.summary.json"

	local verdict=PASS
	[ ${#fails[@]} -eq 0 ] || verdict=FAIL
	verdicts+=("$role: $verdict${fails[*]:+ - ${fails[*]}}")
	summary_rows+=("| $role | $model_a | $heading_a | $memory_a | $write_a/$written | $guard_a | $hooks_a | $heading_b | $memory_b | $resumed_b | $verdict |")
}

for role in "${roles[@]}"; do
	check_role "$role" || verdicts+=("$role: FAIL check aborted")
done

{
	echo "# Launch check"
	echo
	echo "Run $(TZ=Europe/Lisbon date '+%a %Y-%m-%d %H:%M') Lisbon time on $(uname -s) with $(claude --version 2>/dev/null | head -n 1); sandbox $([ "$sandbox" = 1 ] && echo on || echo off); model override: ${model_override:-none}."
	echo
	echo "Launch form: \`claude -p --append-system-prompt-file <body> --settings <fragment> --permission-mode <mode> --strict-mcp-config --mcp-config roles/mcp.json --model <model> --effort <effort>\` with \`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1\`, \`TILLER_SESSION_ID\`, \`TILLER_RUN_ID\` and \`TILLER_ROLE\` set, then the same with \`--resume <session>\`."
	echo
	echo "| Role | Model charged | Heading at start | Memory at start | Write/file | curl probe | Hooks fired | Heading on resume | Memory on resume | Resumed | Verdict |"
	echo "|---|---|---|---|---|---|---|---|---|---|---|"
	printf '%s\n' "${summary_rows[@]}"
	echo
	echo "Expected: heading is the role title, memory NONE (project CLAUDE.md, rules file and auto-memory MEMORY.md all planted and excluded), curl DENIED by the common deny list, Write OK in a worker's worktree, NOTOOL for the reviewer, denied for the orchestrator, SessionStart fired from the fragment, same session id on resume."
	echo
	printf -- '- %s\n' "${verdicts[@]}"
} >"$out/report.md"
jq -s '.' "$out"/*.summary.json >"$out/report.json"

echo
cat "$out/report.md"
echo
echo "check-launch: report in $out"
for v in "${verdicts[@]}"; do
	case "$v" in *": FAIL"*) exit 1 ;; esac
done
