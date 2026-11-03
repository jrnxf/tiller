repo
printf "# count\n" >README.md
commit "Start the line counter"
git push -q origin main
git switch -q -c fm/alpha-72
mkdir -p bin
cat >bin/count.js <<'JS'
// Prints the number of lines in the file named by the first argument.
import { readFileSync } from "node:fs";
const text = readFileSync(process.argv[2], "utf8");
console.log(text === "" ? 0 : text.replace(/\n$/, "").split(/\r?\n/).length);
JS
printf '{ "type": "module" }\n' >package.json
commit "Count lines, including CRLF files"
git push -q -u origin fm/alpha-72
reply card-show <<'R'
alpha-72  ship  "Line counter handles CRLF files"  size S  state in-verification
acceptance:
  1. `node bin/count.js <file>` prints 3 for a three-line file with LF endings
  2. it prints 3 for the same file with CRLF endings
  3. it works the same in Windows PowerShell
effective_delivery: direct-pr
runs: builder-run-2 (builder, done, branch fm/alpha-72, PR #97 open)
R
echo "builder-run-2  builder  done  branch fm/alpha-72  worktree: this directory" | reply run-show
