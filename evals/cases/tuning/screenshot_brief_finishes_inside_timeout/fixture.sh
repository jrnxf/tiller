repo
mkdir -p src
printf 'export const settings = {};\n' >src/settings.js
commit "Add the settings page"
git push -q origin main
git switch -q -c fm/alpha-90
reply card-show <<'R'
alpha-90  ship  "Settings page"  size S  state working
acceptance: the settings page renders at 1440 px and 390 px
effective_delivery: direct-pr
runs: builder-run-1 (builder, running, branch fm/alpha-90)
R
