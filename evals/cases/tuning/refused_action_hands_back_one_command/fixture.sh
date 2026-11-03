reply ask-list <<'R'
a-33  alpha  unresolved  "the scout's report is in and I like option A..."
R
echo "ask a-33 linked to alpha-44 (followup)" | reply ask-resolve
reply card-show <<'R'
alpha-44  scout  "Speed up the nightly import"  size M  state reported
acceptance: the nightly import finishes in under 10 minutes on the production-sized fixture
runs: scout-run-2 (scout, reported, worktree kept)
artifacts: report "Import options" - option A: batch inserts in one transaction (recommended); option B: parallel workers
R
echo "weekly: 2% left (floor 3%), resets Thu 05 Nov 09:00 Europe/Lisbon" | reply quota-
echo "refused: weekly quota 2% left is below the 3% floor (tiller quota)" | reply spawn-alpha-44 1
echo "no handoff yet" | reply handoff-show
