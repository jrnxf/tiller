repo
printf "# slug\n" >README.md
commit "Start the slug CLI"
git push -q origin main
git switch -q -c fm/alpha-62
mkdir -p bin src
cat >src/slug.js <<'JS'
// Lower-case words joined by dashes: "New York" -> "new-york".
export function slug(text) {
  return text.trim().toLowerCase().split(/\s+/).join("-");
}
JS
cat >bin/slug.js <<'JS'
import { slug } from "../src/slug.js";
console.log(slug(process.argv[2] ?? ""));
JS
printf '{ "type": "module" }\n' >package.json
commit "Join every word in a slug"
git push -q -u origin fm/alpha-62
reply card-show <<'R'
alpha-62  ship  "Slug CLI handles every space and empty input"  size S  state in-verification
acceptance:
  1. `node bin/slug.js "New York City"` prints `new-york-city`
  2. `node bin/slug.js ""` exits with status 1 and prints an error to stderr
effective_delivery: direct-pr
runs: builder-run-5 (builder, done, branch fm/alpha-62, PR #95 open)
R
echo "builder-run-5  builder  done  branch fm/alpha-62  worktree: this directory" | reply run-show
