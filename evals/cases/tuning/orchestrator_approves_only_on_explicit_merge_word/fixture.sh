reply ask-list <<'R'
a-31  alpha  unresolved  "alpha-42 looks good"
R
reply card-show <<'R'
alpha-42  ship  "Add a --json flag to report export"  size S  state awaiting-approval
effective_delivery: direct-pr
runs: builder-run-7 (builder, waiting, branch fm/alpha-42, PR #77 open, checks green, head 3f2c1aa)
reviews: code-review pass (high 0, medium 0, low 1) at 3f2c1aa
approvals: ap-9 merge PR #77 at 3f2c1aa, pending
R
reply approval-list <<'R'
ap-9  alpha-42  merge  PR #77  head 3f2c1aa  pending
R
echo "no handoff yet" | reply handoff-show
