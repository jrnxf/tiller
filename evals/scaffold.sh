#!/usr/bin/env bash
# Print the scaffold script for one case: it installs the stub tiller and the
# fixture helpers in the run's empty workspace, then runs the case's
# fixture.sh. run.sh ships it with the case; check.sh runs it offline.
#
# Usage: evals/scaffold.sh <case dir>
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
src=${1:?usage: evals/scaffold.sh <case dir>}

printf '#!/usr/bin/env bash\nset -euo pipefail\nmkdir -p .tiller/bin .tiller/replies\n'
printf "cat >.tiller/bin/tiller <<'TILLER_STUB'\n"
cat "$here/stub/tiller"
printf 'TILLER_STUB\nchmod +x .tiller/bin/tiller\n'
cat "$here/fixture-lib.sh"
[ ! -f "$src/fixture.sh" ] || cat "$src/fixture.sh"
