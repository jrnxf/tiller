# Releasing

Tiller is private, so a release is a tag and nothing else: no GitHub release, no release binaries, no Homebrew formula. Consumers pin the tag through the Nix flake, for example `git+https://github.com/jrnxf/tiller?ref=refs/tags/v0.1.0-pre.1`.

## Versions

- `Cargo.toml` keeps the base version, `0.1.0`.
- A tag `v0.1.0-pre.N` (N from 1, no leading zero) is a pre-release. The Nix build stamps `0.1.0-pre.N` into Cargo.toml and Cargo.lock, so `tiller --version` and `tiller version` print it.
- An unstamped Nix build prints `0.1.0-dev+<rev>`. A plain `cargo build` prints `0.1.0`.
- Full release tags (`v0.1.0`) are rejected until tiller goes public.

## Why a stamp file

A flake sees the commit it was fetched at, never the tag name. So the commit a tag points at holds its version in `PRERELEASE`. This is one small commit per pre-release, unlike the tap-based projects where the tag alone carries the version.

## Tagging

1. Wait for `ci.yml` to pass on the main commit you want to ship.
2. Set `PRERELEASE` to the version, for example `0.1.0-pre.2` (no `v`), and push that commit to main.
3. Wait for `ci.yml` to pass on it, then tag and push that exact commit: `git tag v0.1.0-pre.2 <sha> && git push origin v0.1.0-pre.2`.
4. Empty `PRERELEASE` and push, so later commits report `-dev`.

`.github/workflows/tag.yml` runs on every pushed tag. It fails the tag when the name is not valid, `PRERELEASE` does not match, the commit is not on main, or `ci.yml` fails on it. It builds and publishes nothing.

## Check a tag

`nix run 'git+https://github.com/jrnxf/tiller?ref=refs/tags/v0.1.0-pre.2' -- --version` prints `tiller 0.1.0-pre.2`.
