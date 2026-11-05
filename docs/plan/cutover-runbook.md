# Cutover runbook

How one host moves from the old fleet system (Firstmate) to Tiller. It covers T6.1, T6.3 and T6.4 in `build-plan.md`; `checklist.md` tracks the ticks. The owner decided this on Thursday 2026-11-05 (Lisbon time):

- The old system's freeze is a local marker file plus manual stop steps. Nothing in the old system's repository changes.
- Hosts go in this order: the Linux guest first, the personal Mac second, the work Mac last.
- The exporter is checked locally. There is no CI job for it.

Run the steps in order, on one host at a time. Every step says how to tell it worked. If a check fails, stop and use the rollback section.

`<old-home>` is the old system's home directory on the host. `<bundle>` is the new export directory.

## 0. Before you start

1. Read the gate: every kept parity row for this host is `passing` or live-checked (`docs/design/parity.md`, "Gate status for the first cutover host").
2. Confirm the owner said go for this host (CP7).
3. Confirm the owner-only setup in `checklist.md` ("One-time owner setup") is done for the repos on this host, or that those repos run as `detect` on purpose.
4. Take a backup of Tiller's own database if it already holds cards: `tiller backup`. Expect exit 0.

## 1. Freeze the old system's tiller-bound dispatch

The freeze has two parts: a marker file that says "frozen", then manual stops that make it true. Nothing in the old system reads the marker; the stops are what hold, so do both.

1. Write the marker. The path is `<old-home>/state/.tiller-cutover-freeze`. Put the date and time in Lisbon time in it:

   ```sh
   TZ=Europe/Lisbon date '+frozen %Y-%m-%d %H:%M %Z' > <old-home>/state/.tiller-cutover-freeze
   ```

2. Stop the old system's supervisor and watcher processes, so nothing new starts:
   1. Stop the supervise daemon (`bin/fm-supervise-daemon.sh` in `<old-home>`).
   2. Stop the watcher (`bin/fm-watch.sh` in `<old-home>`).
   3. Stop any scheduled job or timer that calls a spawn script.
3. Do not spawn new tiller-bound tasks by hand. Anything the captain wants started now waits for Tiller.
4. Check: no old-system supervisor, watcher or spawn script is running. For example `pgrep -fl 'fm-supervise|fm-watch|fm-spawn'` prints nothing.
5. Check Tiller's side: `tiller owner show` reports no owner, or `firstmate`. If Tiller already runs on this host and may dispatch, set the guard now: `tiller owner set firstmate`. A real import is refused while the marker says `firstmate`, so step 4 flips it to `tiller` before the real import.

The marker is one-sided: only people read it. Tiller does not, and the old system does not. Anything left running that the stops missed can still dispatch, so the check in item 4 is the real gate.

## 2. Drain in-flight work

1. List what is running: the old system's fleet snapshot, or its backlog "In flight" section.
2. For each task, either wait until it lands, or record it in `CUTOVER.md` as `adopt` or `respawn`. Do not leave a running task unrecorded: it imports as a queued card and can dispatch twice.
3. Holds that need the captain stay as holds. After the import, any blocked hold whose dependency is already done must be re-applied by hand with `tiller card hold <id> --until <time> --reason "..."` (the importer turns it into a dependency only).
4. Check: the backlog "In flight" section is empty, or every entry is recorded in `CUTOVER.md`.

## 3. Check the exporter (local, no CI job)

Run it from the repository root, after the build, on the host being cut over:

```sh
nix build
nix develop -c scripts/firstmate-export-check.sh result/bin/tiller
```

Pass criteria, both required:

1. The command exits 0.
2. Its last line is exactly `ok: the exporter bundle passes tiller import --dry-run and a real import`.

Any other output is a fail. Do not run the export until it passes. It takes seconds, and it writes only into scratch directories it removes. It never touches `~/.tiller` or the old home. This check stays local on purpose: it is not in `.github/workflows/ci.yml`.

## 4. Export, import, flip the owner

1. Export the old home with the final mode:

   ```sh
   scripts/firstmate-export \
     --home <old-home> \
     --out "$HOME/.tiller/import/<label>-$(TZ=Europe/Lisbon date +%Y%m%d-%H%M)-final" \
     --host-label <label> \
     --mode final
   ```

   Exit 0 means the bundle is written. Exit 3 means the secret scan found a hit nobody allowed: read `secret-scan.md` in the bundle, and allow a hit only if it is a test fixture or a document.
2. Read `README.md`, `open-questions.md` and `secret-scan.md` in the bundle. Settle the open questions.
3. Dry run: `tiller import --dry-run <bundle>`. Expect exit 0 and the card, project and memory counts you expect.
4. Flip the owner: `tiller owner set tiller`, then `tiller owner show` reports `tiller`.
5. Real import: `tiller import <bundle>`. Expect exit 0. Run it a second time: it reports `already_imported: true`.
6. Verify with `tiller doctor` (all checks ok). Compare counts with `tiller card show` or the database, not `tiller card list`, which shows only open and recently closed cards.
7. Review the memory proposals (`tiller mem`): approve the rules and requirements first, then the notes by project. They are not active until approved.

## 5. Repoint, start, delete the demo

1. Open the dotfiles PR that replaces the old system's launch, notification and dispatch wiring with the Tiller home-manager module. The owner reviews and merges it; do not merge it yourself.
2. Start the services (the module's launchd agent on a Mac, the systemd user unit on Linux). `tiller doctor` is ok and the board answers on `http://127.0.0.1:7421`.
3. Delete the throwaway demo (T0.5).

## 6. Hosts and order

| Order | Host | Plan item | When |
|---|---|---|---|
| 1 | Linux guest | T6.3 | First. CP7 go for this host |
| 2 | Personal Mac | T6.1 | After 2-3 stable days on the guest |
| 3 | Work Mac | T6.4 | Last. Its data stays on that machine; set `federation.accept = false` unless the owner chose otherwise |

Run sections 0 to 5 once per host. Pair the hosts with `tiller peer add` only after the second host is stable. After each flip, watch for 48 hours (T6.2): the daemon checks the rollback triggers at no token cost.

## 7. Rollback

Triggers: lost work, an unauthorized action, a duplicate dispatch. Within two weeks of a host's flip:

1. Stop the Tiller services on the host. Tiller agents it already started keep running until you stop them, so list and stop them too.
2. Hand ownership back: `tiller owner set firstmate`. Tiller now refuses real imports and card changes.
3. Take a backup of what Tiller holds: `tiller backup`. Keep it.
4. Remove the freeze: delete `<old-home>/state/.tiller-cutover-freeze`.
5. Restart the old system's supervise daemon and watcher, the ones stopped in section 1.
6. Revert the dotfiles PR.
7. Check that the old backlog and in-flight list match what you expect, and re-create by hand any task that Tiller finished while it owned the host.
8. To restore Tiller's own database from a backup instead (for example after a bad import), do it before step 2, while the marker still says `tiller`: `tiller restore --dry-run`, then `tiller restore`. It moves the live database aside to `state.db.pre-restore-<stamp>`, and it refuses once the marker says `firstmate`.

The old system is stopped, not deleted, for two weeks after the last flip (CP8).
