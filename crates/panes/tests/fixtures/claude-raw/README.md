# Raw Claude Code recordings

Nine sessions of Claude Code 2.1.312 (`claude --model haiku --permission-mode default`), recorded
with `tiller holder record` (test-only `record` feature). About 19 minutes in total.

Each recording is two files:

- `<name>.raw`: every byte the child wrote to its PTY, in order.
- `<name>.json`: the sidecar. `cols` and `rows` give the start size. `resizes` lists each later
  size change. `marks` lists the frames to check.

Every offset counts bytes of the `.raw` file. A resize at offset N happens after byte N-1 and before
byte N. A mark at offset N is read after the first N bytes. Marks sit on frame ends (after
`ESC [ ? 2026 l`) that a person checked by eye.

A mark holds:

- `composer`: what `tiller_panes::composer::classify` returns (`Empty`, `Busy`, `Dialog`,
  `Unknown` or `{"Holds": "<text>"}`).
- `prompt`: `null`, or the permission or trust dialog that `tiller_daemon::prompt::read` gives, with
  the tool (`Bash`, `Write`, `Edit` or `input`) that read it. The screen hash is left out.
- `note`: what the frame shows.

`crates/cli/tests/raw_gate.rs` replays each recording through the holder's screen model and checks
every mark. It fails when a verdict changes. Do not refresh a sidecar to make it pass. If the
verdict is wrong, fix the code.

## States

| Recording | States |
|---|---|
| `trust-and-popup` | trust dialog, idle composer, typed draft, cleared draft, slash popup, `/exit` |
| `short-and-long-reply` | one-word reply, 120-line scrolling reply, `/exit` |
| `write-dialog-and-sleep` | Write permission dialog, 20 s Bash run |
| `edit-and-bash-dialogs` | Edit permission dialog, Bash permission dialog, a declined tool |
| `resize` | 40x30 to 120x40 to 40x30 to 120x40 around a 400-line reply |
| `idle-dwell` | two-file read, long idle stretch, second turn |
| `task-list` | six-item task list under the spinner (shows five plus "+1 pending") |
| `queued-message` | message typed while a turn runs, queued row and `ctrl+enter to send now` |
| `sleep-then-essay` | Bash run followed by a 200-word reply |

## Known limits

- While a reply streams, Claude Code can draw no spinner row for about two seconds. `classify`
  reads `Empty` on such a frame, and the screen alone cannot tell it from a finished turn. No mark
  sits on one. `sleep-then-essay` has such a stretch between its second and third marks.
- Right after a resize the screen is not redrawn yet. No mark sits on those frames.
- The working folder was scrubbed to `/home/user/...` by position, so the diff-redrawn fragments
  of the banner's folder line show garbled path pieces. They carry no real name and no verdict
  reads them.
- Account, plan, e-mail and host names are replaced with generic values. The replacements keep
  every byte length, so the offsets in the sidecars stay valid.
