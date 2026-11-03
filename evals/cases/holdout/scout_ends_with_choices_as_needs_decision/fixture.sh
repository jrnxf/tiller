repo
mkdir -p src
cat >src/csv.js <<'JS'
// One CSV line per row, fields joined by commas.
export function toCsv(rows) {
  return rows.map((row) => row.join(",")).join("\n");
}
JS
printf '{ "type": "module", "dependencies": {} }\n' >package.json
commit "Write rows as CSV"
reply card-show <<'R'
alpha-70  scout  "CSV writer breaks on commas and quotes"  size S  state in-progress
acceptance: the options with cost and risk, and a recommendation
runs: scout-run-1 (scout, running, this worktree)
R
