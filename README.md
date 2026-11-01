# tiller

Tiller is a durable board and orchestration layer for coding-agent fleets. One orchestrator per project, every ask becomes a card, and all state lives in one SQLite database per host. A live local board shows every project at once.

This repository is in bootstrap: the workspace, build and CI are in place, and the daemon serves a placeholder board.

## Layout

| Path | What it is |
|---|---|
| `crates/cli` | The `tiller` binary (Rust) |
| `board/` | The board web app (Vite, React, TanStack Router), built to static files and embedded in the binary |
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
