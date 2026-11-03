repo
mkdir -p src
printf 'export function rows() {\n  return Array.from({ length: 10 }, (_, i) => i + 1);\n}\n' >src/rows.js
commit "List the export rows"
git push -q origin main
git switch -q -c fm/alpha-51
cat >src/paginate.js <<'JS'
// Returns page `page` (1-based) of `items`, `size` items per page.
export function paginate(items, page, size) {
  const start = page * size;
  return items.slice(start, start + size);
}
JS
commit "Paginate the export list"
git push -q -u origin fm/alpha-51
reply card-show <<'R'
alpha-51  ship  "Paginate the export list"  size M  state in-review
acceptance: page 1 holds the first `size` rows; the last page holds the remainder; no row is skipped or repeated
effective_delivery: direct-pr
runs: builder-run-3 (builder, running, branch fm/alpha-51, merge base main)
R
echo "builder-run-3  builder  running  branch fm/alpha-51  worktree: this directory" | reply run-show
