# AGENTS.md

Tiller is a Rust core (CLI, daemon, state) plus a TypeScript board (Vite, React, TanStack Router). Both halves ship as one binary: the board is built to static files and embedded in `tiller`.

## Hard rules

1. **Public by default.** Write the repo as if it will become public. Nothing committed or posted may contain internal hostnames, LAN addresses, absolute local paths, machine names, personal file names, or the names of private projects. Tests and examples use `example.com`, `192.0.2.x` and `/home/user`.
2. **No secrets.** Never read `.env`, `.envrc`, key files, `*.pem`, `*.key` or vault output. When a command needs a secret, ask a human to run it.
3. **Install only through Nix.** Never use `brew install`, `npm i -g` or `cargo install` ad hoc. Tools come from the flake dev shell, and Tiller ships as a Nix flake package plus a home-manager module.
4. **Delivery path.** One task is one branch, one worktree and one PR. Get an independent `/code-review` of the diff before the PR is opened. `no-mistakes` is the shipping gate, not a substitute for review. Merge only when the owner says so.
5. **Writing style.** Never use the em dash; use "-". Never credit an agent in commits or PRs: no `Co-Authored-By` line and no "Generated with" line. Show every date in Europe/Lisbon time with its weekday.
6. **Quality.** Fix any lint error, test failure or flaky test you see, even when your change did not cause it. Treat UI as pixel-exact: check screenshots in both themes at 1440 px and 390 px wide.
7. **The predecessor system is read-only** during the build, except for steps the owner approves at a checkpoint.
8. **Quota.** Check quota before starting a wave of work. Running ahead of pace is not a reason to stop. Hold the wave and tell the owner only when the week would run out before the work could finish, or when under 10% of it is left; otherwise start and mention the pace in the note. Away mode's `--quota-floor` still binds.
9. **Never trade output or quality for speed.** If a shortcut would drop a planned capability or weaken a quality gate (review, tests, `no-mistakes`, bound approvals), stop and ask.

Stop and ask when a contract in a spec has to change, a task fails its acceptance criteria twice, a test flakes and the cause is not in this repo, or anything would write outside a normal PR (dotfiles, GitHub settings, another repo).

## Rust conventions

- Rust stable, edition 2024, pinned in `rust-toolchain.toml` and provided by the dev shell (no rustup).
- clippy runs with `-D warnings`. `unwrap_used` and `expect_used` are denied outside tests.
- `thiserror` in libraries, `anyhow` only in the binary.
- Tests run with `cargo nextest`. Snapshot CLI JSON with insta and drive property tests with proptest once those crates are added.

## Commands

Everything below runs inside `nix develop` (or prefix with `nix develop -c`).

Rust, from the repo root:

| Task | Command |
|---|---|
| Build | `cargo build` |
| Format | `cargo fmt --all` (check: `cargo fmt --all --check`) |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Test | `cargo nextest run --workspace` |
| Run the daemon | `cargo run -- daemon` (the board on `http://127.0.0.1:7421`) |

Board, from the repo root (`--cwd board`) or inside `board/`:

| Task | Command |
|---|---|
| Install | `bun install --cwd board --frozen-lockfile` |
| Dev server | `bun run --cwd board dev` |
| Build | `bun run --cwd board build` (writes `board/dist`, which release builds embed) |
| Lint | `bun run --cwd board lint` (oxlint) |
| Format | `bun run --cwd board format` (check: `format:check`, oxfmt) |
| Typecheck | `bun run --cwd board typecheck` |
| Unused code | `bun run --cwd board knip` |
| Test | `bun run --cwd board test` (vitest) |

Nix:

| Task | Command |
|---|---|
| Build the binary | `nix build` |
| Run the Nix checks (clippy, fmt, nextest, the binary, `bunNixFresh`) | `nix flake check` |
| Check that every output of every listed system evaluates, without a build | `nix flake check --no-build --all-systems` |
| Smoke test the embedded board | `scripts/smoke.sh result/bin/tiller` |
| Regenerate `board/bun.nix` after changing board dependencies | `bun run --cwd board nix:sync` |
| Pixel diff two screenshots | `odiff a.png b.png diff.png` |

The full gate is the CI workflow (`.github/workflows/ci.yml`): its `rust` and `board` jobs run the Rust and board commands above, its `nix` job runs `bunNixFresh`, `nix flake check --no-build --all-systems`, `nix build` and `scripts/smoke.sh`, its `scrub` job runs `scripts/scrub-check.sh`, and its `evals` job runs the offline `evals/check.sh`. The role behavior evals call real models, so they run only on demand: the `Evals` workflow (`.github/workflows/evals.yml`), or `nix develop .#evals -c evals/run.sh` locally (`evals/README.md`). `nix flake check` does not run the board checks, the smoke test or the scrub check.

`board/bun.nix` and `board/src/routeTree.gen.ts` are generated. Never edit them by hand; the `bunNixFresh` flake check fails when `bun.nix` is stale.

## Notes

- bun 1.4 writes `lockfileVersion` 2, which the released `bun2nix` rejects. The dev shell and the `bunNixFresh` check use `bun2nix` with `nix/patches/bun2nix-lockfile-v2.patch` (upstream PR 110). Drop the patch once a release accepts lockfile v2.
- `tiller daemon` binds localhost only.
- `alacritty_terminal`, the terminal holder's screen model, is pinned with `=0.26.0` in `Cargo.toml` once `tiller-holder` adds it. A bump is its own task, and it must pass the fixture gate (`docs/design/terminal-holder.md` section 9.1).
