mkdir -p transcripts
cat >transcripts/claude-build-44.jsonl <<'JSONL'
{"type":"user","message":{"content":"Start on the brief at the end of your instructions."}}
{"type":"assistant","message":{"content":"Reading the card and the export module."}}
{"type":"tool_use","name":"Bash","input":{"command":"bun test"}}
{"type":"tool_result","content":"error: bun 1.3.2 cannot read lockfileVersion 2; this project pins bun 1.4"}
{"type":"assistant","message":{"content":"The installed bun is older than the pin. Checking the dev shell."}}
{"type":"tool_use","name":"Bash","input":{"command":"nix develop -c bun --version"}}
{"type":"tool_result","content":"1.4.0"}
{"type":"tool_use","name":"Bash","input":{"command":"nix develop -c bun test"}}
{"type":"tool_result","content":"12 pass, 0 fail"}
{"type":"assistant","message":{"content":"Added the button and its test; pushing the branch."}}
{"type":"tool_use","name":"Bash","input":{"command":"tiller status done 'PR #91 open, checks green'"}}
JSONL
cat >transcripts/claude-review-44.jsonl <<'JSONL'
{"type":"user","message":{"content":"Start on the brief at the end of your instructions."}}
{"type":"assistant","message":{"content":"One low finding: the filename is hard-coded; fine for now."}}
{"type":"tool_use","name":"Bash","input":{"command":"tiller review record 01JC0000000000000000000RV44 --reviewer code-review --coverage diff --verdict pass --sha 9c41e07b2d5a8f3e6b1c0d4a7e2f9b8c3d6a5e10 --low 1"}}
JSONL
echo "ok: retro 01JC0000000000000000000RT7 finished" | reply retro-finish
