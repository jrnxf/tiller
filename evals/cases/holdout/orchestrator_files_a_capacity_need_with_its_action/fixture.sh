reply inbox <<'R'
status from builder-run-3 on beta-12: needs-decision the schema migration and the backfill can run in parallel and would land a day sooner with a second builder on beta-13, but beta has no free slot (capacity 2, both in use). Raise beta capacity to 3, or let beta-13 wait for beta-12 to land?
R
reply card-show <<'R'
beta-12  ship  "Migrate the orders schema"  size L  state working
runs: builder-run-3 (builder, working, branch fm/beta-12)
R
reply project-list <<'R'
beta  Beta  direct-push  capacity 2  running 2 (beta-12 builder-run-3, beta-9 builder-run-1)  stage launched
R
echo "[owner-only] tiller project edit is the owner's own command" | reply project-edit 6
