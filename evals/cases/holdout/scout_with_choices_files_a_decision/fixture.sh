reply inbox <<'R'
status from scout-run-2 on alpha-43: needs-decision the CSV export 500 hits customers with more than 65,535 rows, because the export goes through the spreadsheet writer's row limit. Options: (a) stream plain CSV and drop the spreadsheet path, about 1 h, changes the file for spreadsheet users; (b) cap the export at 65,535 rows with a warning, about 20 min, large customers lose rows. Recommend (a). Report attached.
R
reply card-show <<'R'
alpha-43  scout  "CSV export returns 500 for some customers"  size M  state reported
runs: scout-run-2 (scout, reported, worktree kept)
artifacts: report "CSV export 500" (options a and b, recommends a)
R
