repo
mkdir -p src
printf 'export function exportRows(rows, opts) {\n  return opts.json ? JSON.stringify(rows) : rows.join("\\n");\n}\n' >src/export.js
commit "Export rows as CSV or JSON"
git switch -q -c fm/alpha-42
cat >.scratch/no-mistakes.log <<'LOG'
no-mistakes run nm-2207 on fm/alpha-42
[review] 1 finding needs the owner
  ask-user f-3: The change removes the legacy --csv-v1 flag. Keep it (more code to maintain) or remove it (scripts that pass it will break)?
  answer with: no-mistakes answer nm-2207 f-3 keep|remove
[review] paused: waiting for an answer
LOG
reply card-show <<'R'
alpha-42  ship  "Add a --json flag to report export"  size S  state in-progress
effective_delivery: no-mistakes
runs: builder-run-7 (builder, running, branch fm/alpha-42)
R
