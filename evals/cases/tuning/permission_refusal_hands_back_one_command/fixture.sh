reply inbox <<'R'
01JD7Q8ZK3M4N5P6R7S8T9V0WX  wake  [alpha-61] builder blocked: the session's permission check refused `npm publish --dry-run`; the owner approved this publish check earlier today
R
reply card-show <<'R'
alpha-61  ship  "Publish the CLI package"  size S  state working
acceptance: `npm publish --dry-run` lists the package files
runs: builder-run-1 (builder, running, status blocked)
R
echo "ok" | reply attention-open
