# [M] Audit dependencies

## Goal

Run the `deps` audit: `quest audit deps` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the previous run covered releases through 2026-11-22.
- `Cargo.lock` is not committed, so `cargo upgrade --dry-run --incompatible` (from `cargo-edit` in the dev shell) lists the only Rust bumps that need work: the semver-incompatible ones.
- The previous run found Dependabot's uv entry, then at `/`, had never run a version update job, and pointed it at `/py/web-transport`; first confirm that entry and the new Gradle one list recent jobs at https://github.com/moq-dev/web-transport/network/updates.
- Dependabot covers Cargo, Bun, GitHub Actions, uv (`/py/web-transport`), and Gradle (`/kt`); check first what it cannot: the flake's tool versions (`nodejs_24`, `python312`, `wasm-bindgen-cli_0_2_130`, which pins with the `wasm-bindgen` harness dev-dependency), CI toolchain versions (`node-version`, `java-version`, `python-version`), `swift-tools-version`, and the `rustls-platform-verifier-android` pin shared by `rs/web-transport-ffi/Cargo.toml` and `kt/gradle.properties`.
- `cargo audit` reports unmaintained transitive crates; the previous run traced `basic-toml` to `uniffi` and `chrono`, `humantime`, and `yaml-rust` to `tokio-quiche` 0.20. Recheck them against the `tokio-quiche` the workspace then uses.
- `rind` and `rind-rustls` are maintained in moq-dev/rind; plan their fork work there, not here.

## Audit

- deps

## Required

- [After m0](/quest/m1/README.md) - runs once per milestone

## Scheduled

- 2026-11-29 - one week after the previous run
