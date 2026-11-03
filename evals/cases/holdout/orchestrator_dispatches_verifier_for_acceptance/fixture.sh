reply ask-list <<'R'
a-37  alpha  unresolved  "the builder says it's finished and the PR is green..."
R
echo "ask a-37 linked to alpha-60 (followup)" | reply ask-resolve
reply card-show <<'R'
alpha-60  ship  "Retry failed webhook deliveries"  size M  state in-review
acceptance:
  1. a delivery that fails with a 5xx is retried 3 times with backoff
  2. a 4xx is not retried
  3. each attempt is listed on the webhook's history page
effective_delivery: direct-pr
runs: builder-run-9 (builder, done, branch fm/alpha-60, PR #91 open, checks green)
reviews: code-review pass (high 0, medium 0, low 0)
R
echo "weekly: 47% left (floor 3%)" | reply quota-
echo "no handoff yet" | reply handoff-show
