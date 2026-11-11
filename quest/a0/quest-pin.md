# [XS] Bump the dev shell's quest

## Goal

Local merges auto-resolve quest link lists again. The configured `merge.quest.driver` calls `quest resolve ... --marker-size=%L`, but `flake.nix` pins `kixelated/quest@ed657ba` (0.1.0), which rejects that flag (`unknown option '--marker-size=7'`), so every merge of a quest branch falls back to manual link-list resolution.

## Plan

Bump the `quest` flake input to a current `kixelated/quest` main commit and update `flake.lock`. Verify `quest resolve --help` accepts `--marker-size`, that `quest check` passes on the tree, and that a local merge with a link-list conflict auto-resolves. Newer quest can also upgrade old merge drivers and keep its pin current (kixelated/quest#427); adopt that only if it needs no extra repository configuration. Then check that the `Quest` workflow still passes with the new version.
