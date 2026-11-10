# tiller

Tiller is a durable board and orchestration layer for coding-agent fleets. One orchestrator per project, every ask becomes a card, and all state lives in one SQLite database per host. A live local board shows every project at once.

The `tiller` CLI and the board work on the same state. The CLI writes `state.db` directly, and the daemon serves the live board and its API on localhost. The board shows lanes, cards, the Inbox, agents, memory, health and settings, and the owner can act from it. The CLI covers the same actions and adds the agent tools. `docs/design/cli-board-parity.md` lists what each surface can do.

## Layout

| Path | What it is |
|---|---|
| `crates/cli` | The `tiller` binary (Rust) |
| `crates/core` | `state.db`: schema and migrations, ids, domain types, and every state mutation |
| `crates/daemon`, `herdr`, `delivery`, `observe`, `reconcile`, `agents`, `replay` | The daemon and its parts: board API, herdr client, outbox delivery, observation, reconcile, agent lifecycle, replay |
| `board/` | The board web app (Vite, React, TanStack Router), built to static files and embedded in the binary |
| `docs/` | Design docs, the build plan and checklist, and ADRs |
| `scripts/` | The smoke test and the scrub check |
| `probe/` | The read-only host capability probe and its reports |
| `demo/` | The throwaway board demo, deleted at cutover |
| `nix/` | Home-manager module and Nix patches |
| `flake.nix` | Package, dev shell and checks |

## Try it

```sh
nix build
./result/bin/tiller --version
./result/bin/tiller daemon        # serves the board on http://127.0.0.1:7421
```

## Develop

```sh
nix develop                       # Rust toolchain, bun, nodejs, sccache, sqlite3, cargo-nextest
cargo nextest run --workspace
bun install --cwd board
bun run --cwd board build         # the daemon embeds board/dist in release builds
cargo run -- daemon
```

`AGENTS.md` has the full command list and the rules for contributing.

## License

MIT
