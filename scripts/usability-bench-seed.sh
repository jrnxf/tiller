# shellcheck shell=bash
# The bench's seed data (scripts/usability-bench.sh sources this file and calls
# `seed`): 14 invented projects, 8 on main and 6 on peer, with cards in every
# column, live and blocked runs, open asks, questions for the owner, a past
# away period and one unhealthy host. Every name here is invented.

# run HOST CARD ROLE STATE TEXT: a run on the card whose worker reported
# `STATE TEXT`. Prints the worker's session.
run() {
  local host=$1 card=$2 role=$3 state=$4 text=$5 worker
  q "$host" spawn "$card" "$role"
  worker=$(session "$host" "${card%-*}" "$card")
  [ -n "$worker" ] || { echo "no worker session for $card on $host" >&2; exit 1; }
  qas "$host" "$worker" status "$state" "$text"
  echo "$worker"
}

# ask HOST PROJECT TEXT: prints the new ask's id.
ask() {
  t "$1" --json ask note "$2" --text "$3" | field .data.ask.id
}

seed_projects() {
  local host=$1 entry
  shift
  for entry in "$@"; do
    project "$host" "${entry%%:*}" "${entry##*:}"
    orchestrator "$host" "${entry%%:*}" >/dev/null
  done
}

seed() {
  local c o w a tomorrow next_week
  tomorrow=$(date -v+1d +%Y-%m-%d 2>/dev/null || date -d '+1 day' +%Y-%m-%d)
  next_week=$(date -v+7d +%Y-%m-%d 2>/dev/null || date -d '+7 days' +%Y-%m-%d)

  echo "seeding 8 projects on main" >&2
  seed_projects main harbor:Harbor lantern:Lantern quill:Quill ember:Ember \
    atlas:Atlas juniper:Juniper cobalt:Cobalt meadow:Meadow
  echo "seeding 6 projects on peer" >&2
  seed_projects peer pylon:Pylon tundra:Tundra saffron:Saffron orbit:Orbit \
    willow:Willow basalt:Basalt

  # The owner was away overnight on two projects of main. What follows on
  # those projects happened while away; the period ends at the end of the seed.
  q main away start --until "$tomorrow" --projects quill,ember --merge none \
    --orders "Keep the release branch green. Ask before any schema change."

  # -- main ------------------------------------------------------------------

  # harbor: a busy project. A live builder, a builder that needs a decision,
  # a queue, a parked card and a card that waits for another.
  o=$(session main harbor)
  c=$(card main harbor ship "Add rate limits to the public API" --size M \
    --acceptance "A client gets 429 after 100 requests in a minute" --priority 1)
  run main "$c" builder working "Wiring the limiter into the request pipeline" >/dev/null
  c=$(card main harbor ship "Rotate the signing keys without downtime" --size L --priority 0)
  run main "$c" builder needs-decision "Two rotation plans; need the owner's pick" >/dev/null
  qas main "$o" attention open "$c" --kind decision \
    --question "Rotate keys with a 24 h overlap, or cut over at once?" \
    --recommendation "The 24 h overlap: no client fails, at the cost of one more deploy" \
    --consequence "Overlap: two deploys, no failed requests. Cut over: one deploy, clients with a cached key fail until they refresh." \
    --options '[{"label":"24 h overlap","recommended":true},{"label":"Cut over at once"}]'
  card main harbor ship "Paginate the audit log endpoint" --size S >/dev/null
  card main harbor ship "Return problem+json for every error" --size M --priority 3 >/dev/null
  c=$(card main harbor ship "Move the session store to the new cluster" --size L)
  q main card hold "$c" --until "$next_week" --reason "The new cluster is not provisioned yet"
  w=$(card main harbor ship "Delete the old session store")
  q main card dep add "$w" "$c"
  ask main harbor "What is left before the rate limit work can ship?" >/dev/null

  # lantern: a blocked builder and a credential the owner must supply.
  o=$(session main lantern)
  c=$(card main lantern ship "Send the weekly digest email" --size M)
  run main "$c" builder blocked "The mail provider rejects the sandbox key" >/dev/null
  qas main "$o" attention open "$c" --kind credential \
    --question "The mail provider's sandbox key is rejected. Can you issue a new one?" \
    --recommendation "Issue a key with send-only scope and store it under the project's secrets"
  card main lantern scout "Find why the unsubscribe link 404s on mobile" >/dev/null
  card main lantern ops "Renew the sending domain's DKIM record" --priority 1 >/dev/null

  # quill: a scout that reported while the owner was away, and an answered ask.
  o=$(session main quill)
  c=$(card main quill scout "Compare three markdown parsers for the editor" --size S)
  w=$(run main "$c" scout working "Benchmarking the third parser")
  qas main "$w" status done "Report written: parser B is 3x faster and passes every fixture"
  qas main "$w" done --outcome reported
  qas main "$o" attention open "$c" --kind report \
    --question "The parser report is ready. Promote it to a build on parser B?" \
    --recommendation "Promote: parser B passes every fixture and is the fastest"
  a=$(ask main quill "Does the editor keep undo history across a reload?")
  qas main "$o" ask resolve "$a" chat
  qas main "$o" reply --to "$a" \
    --text "No. Undo history lives in memory only. Persisting it is about one day of work; say the word and I will file a card."
  card main quill ship "Autosave drafts every 10 seconds" --size S >/dev/null

  # ember: a run that failed while the owner was away, with a recovery question.
  o=$(session main ember)
  c=$(card main ember ship "Upgrade the image pipeline to the new codec" --size M)
  w=$(run main "$c" builder working "Converting the fixtures")
  qas main "$w" status failed "The codec library segfaults on 16-bit PNGs"
  qas main "$w" done --outcome failed
  qas main "$o" attention open "$c" --kind recovery \
    --question "The codec upgrade failed on 16-bit PNGs. Retry with the fallback decoder, or drop 16-bit support?" \
    --options '["Retry with the fallback decoder","Drop 16-bit support","Cancel the card"]'
  card main ember ship "Cache thumbnails at the edge" --size M >/dev/null

  # atlas: quiet, with one long queue.
  for title in "Index the places table by geohash" "Add a tile cache warmer" \
    "Expose route duration in the API" "Fix the antimeridian wrap in bounding boxes" \
    "Document the import format" "Drop the legacy v1 routes"; do
    card main atlas ship "$title" >/dev/null
  done

  # juniper: one live builder and work that is done.
  c=$(card main juniper ship "Add dark mode to the settings page" --size S)
  run main "$c" builder working "Checking contrast on the form controls" >/dev/null
  c=$(card main juniper ops "Archive last year's logs" --size S)
  q main card complete "$c" --evidence "Moved 41 GB to cold storage; the bucket lists 0 objects"
  c=$(card main juniper question "Which font do the invoices use?")
  q main card close "$c" --outcome answered --reason "Inter, 10 pt, set in the invoice template"

  # cobalt: an unanswered ask and nothing else running.
  ask main cobalt "Can we ship the pricing page without the annual toggle?" >/dev/null
  card main cobalt ship "Add the annual billing toggle" --size M >/dev/null

  # meadow: a new project with nothing on it yet (the empty state).

  # -- peer ------------------------------------------------------------------

  # pylon: the peer's busy project, the mirror of harbor.
  o=$(session peer pylon)
  c=$(card peer pylon ship "Retry failed webhook deliveries with backoff" --size M --priority 1)
  run peer "$c" builder working "Writing the backoff schedule tests" >/dev/null
  c=$(card peer pylon ship "Sign webhook payloads" --size M)
  run peer "$c" builder needs-decision "HMAC or asymmetric signatures; need the owner's pick" >/dev/null
  qas peer "$o" attention open "$c" --kind decision \
    --question "Sign webhooks with HMAC-SHA256, or with Ed25519?" \
    --recommendation "HMAC-SHA256: every client library verifies it today" \
    --options '[{"label":"HMAC-SHA256","recommended":true},{"label":"Ed25519"}]'
  card peer pylon ship "Show delivery attempts in the dashboard" --size S >/dev/null
  ask peer pylon "How many webhooks failed yesterday, and why?" >/dev/null

  # tundra: a blocked builder on the peer.
  o=$(session peer tundra)
  c=$(card peer tundra ship "Migrate the cold tier to the new bucket layout" --size L)
  run peer "$c" builder blocked "The staging bucket denies list access" >/dev/null
  qas peer "$o" attention open "$c" --kind credential \
    --question "The staging bucket denies list access. Can you grant the migration role read access?"

  # saffron: a finished scout and a queue.
  o=$(session peer saffron)
  c=$(card peer saffron scout "Measure cold start time of the recipe search" --size S)
  w=$(run peer "$c" scout working "Profiling the index load")
  qas peer "$w" status done "Report written: 80% of cold start is the index load"
  qas peer "$w" done --outcome reported
  card peer saffron ship "Lazy-load the search index" --size M >/dev/null
  card peer saffron ship "Add fuzzy matching for ingredient names" --size M >/dev/null

  # orbit: a live builder.
  c=$(card peer orbit ship "Plot the ground track for the next 24 hours" --size M)
  run peer "$c" builder working "Projecting the orbit onto the map tiles" >/dev/null

  # willow: a parked card and an ops card.
  c=$(card peer willow ship "Switch the booking calendar to the new time zone data" --size S)
  q peer card hold "$c" --until "$next_week" --reason "Waits for the tzdata release"
  card peer willow ops "Rotate the calendar API token" >/dev/null

  # basalt: quiet.
  card peer basalt ship "Compress the nightly export" --size S >/dev/null

  # The owner returns: the away period ends, and its brief names what happened.
  q main away end

  # The unhealthy host: the peer's disk floor is far above its free disk, so
  # it refuses every new run until the owner lowers the floor or frees disk.
  # No read command prints the host's rev; it is 1 until the first edit.
  q peer host edit --rev 1 --disk-floor-mb 900000000
}
