# Vendored crates

## alacritty_terminal 0.26.0

`alacritty_terminal/` is the released crate with a small patch, and the root
`Cargo.toml` points `[patch.crates-io]` at it, so `cargo` in the dev shell and
`nix build` both build the patched copy. The version stays pinned at `=0.26.0`.

The holder's live upgrade (`docs/design/terminal-holder.md` section 6.2) must
read every part of a pane's screen state and write it back into a new `Term`.
The released crate keeps five parts private. The patch adds getters for them,
and setters where the escape sequences cannot restore a value exactly:

| State | Getter | Setter |
|---|---|---|
| The inactive grid | `inactive_grid` | `inactive_grid_mut` |
| Both kitty keyboard mode stacks | `keyboard_mode_stack`, `inactive_keyboard_mode_stack` | `set_keyboard_mode_stacks` |
| Tab stops | `tab_stops` | `set_tab_stops` |
| The scroll region | `scroll_region` | `set_scroll_region` |
| The active charset | `active_charset` | (the `Handler` method) |

The patch is `nix/patches/alacritty_terminal-0.26.0-handover.patch`. It adds
methods only, and changes no behavior. The manifest differs from the released
one in two ways: it has no `authors` (the scrub check rejects any email
address), and it has no `[[test]]`, because the reference tests are not
vendored.

The `alacrittyPatchFresh` flake check fetches the released crate, applies the
patch, and fails when the result differs from this directory. After a change
to `alacritty_terminal/src`, write the patch again from the repo root:

```sh
system=$(nix eval --impure --raw --expr builtins.currentSystem)
crate=$(nix build --no-link --print-out-paths ".#checks.$system.alacrittyPatchFresh.crate")
tmp=$(mktemp -d)
tar -xzf "$crate" -C "$tmp"
mkdir "$tmp/a" "$tmp/b"
cp -R "$tmp/alacritty_terminal-0.26.0/src" "$tmp/a/"
cp -R vendor/alacritty_terminal/src "$tmp/b/"
(cd "$tmp" && diff -ru a/src b/src) | sed -E 's/^(---|\+\+\+) ([^\t]+)\t.*/\1 \2/' \
  > nix/patches/alacritty_terminal-0.26.0-handover.patch
```

The same getters and setters go upstream to alacritty, so a later bump can
drop this directory and the patch.
