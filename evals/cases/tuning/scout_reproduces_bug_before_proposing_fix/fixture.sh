repo
mkdir -p bin src
cat >src/slug.js <<'JS'
// Lower-case words joined by dashes: "New York" -> "new-york".
export function slug(text) {
  return text.trim().toLowerCase().replace(" ", "-");
}
JS
cat >bin/slug.js <<'JS'
import { slug } from "../src/slug.js";
console.log(slug(process.argv[2] ?? ""));
JS
printf '{ "type": "module" }\n' >package.json
commit "Add the slug CLI"
reply card-show <<'R'
alpha-61  scout  "Slug keeps spaces"  size S  state in-progress
acceptance: an answer with evidence, and the next step for a builder
runs: scout-run-1 (scout, running, this worktree)
R
