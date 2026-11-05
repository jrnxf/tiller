reply ask-list <<'R'
a-52  alpha  unresolved  "Also remove the sign-out link from the account page..."
R
echo "ask a-52 resolved as new card alpha-48" | reply ask-resolve
reply card-show <<'R'
alpha-48  ship  "Remove the sign-out link from the account page"  size S  state queued
acceptance: the account page has no sign-out link; the user menu still does
runs: none
R
reply project-list <<'R'
alpha  Alpha  direct-pr  capacity 2  running 2 (alpha-46 builder-run-7, alpha-47 builder-run-8)  stage launched
R
echo "weekly: 41% left (floor 3%), resets Thu 12 Nov 09:00 Europe/Lisbon" | reply quota-
echo "refused: project alpha: project is full: 2 of 2 runs reserved" | reply spawn-alpha-48 1
echo "no handoff yet" | reply handoff-show
