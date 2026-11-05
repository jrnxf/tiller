reply ask-list <<'R'
a-40  alpha  unresolved  "alpha-45 has waited in Queued for an hour..."
R
echo "ask a-40 linked to alpha-45 (followup)" | reply ask-resolve
reply card-show <<'R'
alpha-45  ship  "Cache the export manifest"  size S  state queued
acceptance: a repeated export reads the manifest once
runs: none
R
reply project-list <<'R'
alpha  Alpha  direct-pr  capacity 1  running 1 (alpha-44 builder-run-9)  stage launched
R
echo "weekly: 41% left (floor 3%), resets Thu 12 Nov 09:00 Europe/Lisbon" | reply quota-
echo "refused: alpha is at capacity (1 of 1 running: alpha-44 builder-run-9)" | reply spawn-alpha-45 1
echo "[owner-only] tiller project edit is the owner's own command" | reply project-edit 6
echo "no handoff yet" | reply handoff-show
