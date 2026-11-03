reply ask-list <<'R'
a-29  alpha  unresolved  "the PR for the paginated export is up (#88)..."
R
echo "ask a-29 linked to alpha-51 (followup)" | reply ask-resolve
reply card-show <<'R'
alpha-51  ship  "Paginate the export list"  size M  state in-progress
acceptance: page 1 holds the first `size` rows; the last page holds the remainder; no row is skipped or repeated
effective_delivery: direct-pr
runs: builder-run-3 (builder, running, branch fm/alpha-51, PR #88 open, checks green)
reviews: none
R
echo "weekly: 58% left (floor 3%)" | reply quota-
echo "no handoff yet" | reply handoff-show
