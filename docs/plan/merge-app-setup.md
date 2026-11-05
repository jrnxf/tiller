# Merge App setup

How the owner turns on the enforced merge gate (`authority.md` section 6, "Enforced merge gate"; CP2, S03) for one host and its gated repos. `checklist.md` ("One-time owner setup") tracks the ticks.

Only the owner runs these steps. They create a GitHub App, install it and add rulesets, so no agent and no fleet role runs them. Until a repo has passed step 5, its project runs as `detect`, the labelled fallback: merges are detected, not prevented.

`<home>` is the Tiller home: `~/.tiller`, or `$TILLER_HOME` when the host sets it.

## 1. Create the App (once per owner account)

On GitHub: Settings, Developer settings, GitHub Apps, New GitHub App.

| Field | Value |
|---|---|
| GitHub App name | `tiller-merge`. App names are unique across GitHub, so add a suffix if it is taken (for example `tiller-merge-<login>`). |
| Homepage URL | Any URL you own, for example the Tiller repo. |
| Webhook | Clear "Active". Tiller polls; it takes no webhook. |
| Repository permissions | Contents: Read and write. Pull requests: Read and write. Workflows: Read and write (a land that changes `.github/workflows/` needs it). Checks: Read-only. Metadata: Read-only (GitHub sets it). Leave every other permission at "No access". |
| Organization and account permissions | None. |
| Where can this GitHub App be installed? | Only on this account. |

Create it. On the App's General page:

1. Note the App ID (a number such as `1234567`). It is not the Client ID.
2. Under "Private keys", choose "Generate a private key". GitHub downloads one `.pem` file. It is the only copy; GitHub keeps only its fingerprint.

## 2. Install the App on each gated repo

On the App's page, choose "Install App", then your account, then "Only select repositories". Pick each gated repo: every project with delivery `no-mistakes` or `no-mistakes-prod-only`, plus any `direct-pr` or `direct-push` project you want gated (a sandbox project, for example). Add a repo later through the same page ("Configure").

## 3. Add the ruleset on each gated repo

In the repo: Settings, Rules, Rulesets, New ruleset, New branch ruleset.

| Field | Value |
|---|---|
| Ruleset name | `merge gate` (any name; Tiller reports it by name) |
| Enforcement status | Active |
| Bypass list | Add bypass, Apps, the merge App. Set its mode to "Always allow". Nothing else: not the repository admin role, not your own account. Workers use your login, so any other bypass actor lets them through. |
| Target branches | Add target, Include default branch |
| Restrict updates | On |
| Restrict deletions | On |
| Require a pull request before merging | On (zero required approvals is fine; Tiller's approval is the decision) |
| Require status checks to pass | On, with at least one check the repo's CI reports. A rule with no check gates nothing, and Tiller counts it as missing. |
| Block force pushes | On |

Leave the other rules as you like; Tiller only checks that these hold. If more than one ruleset applies to the default branch, each of them must list the App alone as its bypass actor, or the check fails.

Rulesets on a private repo of a personal account need GitHub Pro. A public repo needs no paid plan.

## 4. Place the key and the App ID

Run these on the host, in a terminal of your own (not in an agent session). Replace the download path and the App ID with yours:

```sh
mkdir -p ~/.tiller/forge-app
chmod 700 ~/.tiller/forge-app
echo 1234567 > ~/.tiller/forge-app/app-id
mv /path/to/tiller-merge.*.private-key.pem ~/.tiller/forge-app/private-key.pem
chmod 600 ~/.tiller/forge-app/private-key.pem
```

The file names are fixed: `app-id` holds the numeric App ID, and `private-key.pem` holds the key as GitHub downloaded it. Tiller refuses a key that the group or others can read. The daemon reads both files on each use, so no restart is needed. Every fleet role is denied `<home>/forge-app/` by its deny rules and its sandbox (G20). The key never appears in a prompt, a log line or command output.

Do this step last, after the ruleset exists. The daemon checks every project again within a minute of these files changing; otherwise it checks once an hour.

## 5. Confirm with `tiller doctor`

Within a minute, run `tiller doctor`:

1. The `merge app` check is `ok` and names the App ID. A `warn` names the problem: no key, or a file Tiller cannot use. A `fail` means the key is readable by others: run `chmod 600` on it, and rotate it if anyone else could have read it.
2. Each gated project shows `merge_gate = enforced` under `merge_gates`.

A project that stays `detect` failed the check. The daemon log line `merge gate checked` gives the reason, for example a missing rule or an extra bypass actor. Fix the ruleset; the next hourly check, or a daemon restart, picks it up.

## Later changes

- **A ruleset is edited or removed.** The hourly check moves the project to `detect`, records `host.degraded`, and opens a `recovery` item on `fleet-1` that says why. Restore the ruleset; the next check records `enforced` again.
- **An emergency hand merge.** Edit the ruleset by hand. Reconcile reports the merge like any outside merge (recovery.md R07), and the project drops to `detect` until the ruleset is restored.
- **Rotating the key.** Generate a new key on the App's page, replace `private-key.pem` (mode 600), then delete the old key on GitHub.
- **Removing the App from a host.** Delete `<home>/forge-app/private-key.pem`. Every project on the host moves to `detect` within a minute, each with its recovery item.
