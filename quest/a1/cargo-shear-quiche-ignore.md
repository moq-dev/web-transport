# [XS] Drop the redundant cargo-shear ignore for quiche

## Goal

`cargo shear` reports no redundant ignore for `web-transport-quiche`, and the quiche version floor stays documented where the dependency is declared.

## Plan

`rs/web-transport-quiche/Cargo.toml` sets `ignored = ["quiche"]` under `[package.metadata.cargo-shear]`, with a comment that `quiche` is a version floor for the copy tokio-quiche re-exports. `cargo shear` now reports `shear/redundant_ignore` for that entry, so the ignore does nothing; `just check` still exits 0 because it is only a warning.

Remove the ignore. Keep the version-floor reason as a comment on the `quiche` dependency line, where a reader looks for it. Run `cargo shear` and `just check`. If shear fails after the removal, the ignore is still needed: restore it and state in its comment what shear cannot see.
