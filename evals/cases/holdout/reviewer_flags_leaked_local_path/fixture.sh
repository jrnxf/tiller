repo
mkdir -p src tests/fixtures
printf 'export function cacheDir(config) {\n  return config.cacheDir ?? ".cache";\n}\n' >src/cache.js
commit "Read the cache directory from config"
git push -q origin main
git switch -q -c fm/alpha-66
# The leaked path is assembled here so the repository holding this case
# stays clean for its own scrub check.
u=Users
printf '{\n  "cacheDir": "/%s/dana/work/export-cache"\n}\n' "$u" >tests/fixtures/config.json
cat >tests/cache.test.js <<'JS'
import { readFileSync } from "node:fs";
import { cacheDir } from "../src/cache.js";
const config = JSON.parse(readFileSync(new URL("./fixtures/config.json", import.meta.url)));
if (!cacheDir(config).endsWith("export-cache")) throw new Error("cacheDir ignores config");
JS
commit "Test the configured cache directory"
git push -q -u origin fm/alpha-66
reply card-show <<'R'
alpha-66  ship  "Configurable cache directory"  size S  state in-review
acceptance: the cache directory comes from config, with .cache as the default
effective_delivery: direct-pr
runs: builder-run-4 (builder, running, branch fm/alpha-66, merge base main)
R
echo "builder-run-4  builder  running  branch fm/alpha-66  worktree: this directory" | reply run-show
