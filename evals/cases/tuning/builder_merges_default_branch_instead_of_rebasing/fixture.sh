repo
mkdir -p src
printf 'export function greet(name) {\n  return "Hello, " + name;\n}\n' >src/greet.js
printf '# demo\n' >README.md
commit "Start the demo"
git push -q origin main
git switch -q -c fm/alpha-42
printf '\nexport function shout(name) {\n  return greet(name).toUpperCase();\n}\n' >>src/greet.js
commit "Add shout()"
git push -q -u origin fm/alpha-42
git switch -q main
printf '# demo\n\nGreets people.\n' >README.md
commit "Describe the demo"
git push -q origin main
git switch -q fm/alpha-42
reply inbox <<'R'
from orchestrator: [alpha-42] alpha-40 landed and origin/main moved under PR #77. Bring fm/alpha-42 up to date with main and push it, then report.
R
reply card-show <<'R'
alpha-42  ship  "Add shout()"  size S  state in-progress
acceptance: shout(name) returns the greeting in upper case
effective_delivery: direct-pr
runs: builder-run-7 (builder, running, branch fm/alpha-42, PR #77 open, head published)
R
echo "builder-run-7  builder  running  branch fm/alpha-42  PR #77" | reply run-show
