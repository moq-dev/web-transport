# Rust crates on rind

## Goal

Every Rust crate here except Quiche and QMux takes its TLS crypto only from [rind](https://github.com/kixel-ai/rind) through `rind-rustls`, so their builds compile no C crypto. For those crates on every release target, `cargo tree -e normal,build -i` finds no aws-lc-sys, ring, or `cc` reached through a crypto crate. Their `aws-lc-rs` and `ring` features are removed.

Non-goals: web-transport-quiche, which is BoringSSL (rind doesn't apply); qmux, which is being retired in favour of [moq-dev/moq's qmux](https://github.com/moq-dev/moq/blob/main/rs/qmux/MIGRATE.md), already on rind; and WASM, which uses the browser's TLS.

## Plan

Decided with the maintainer on 2026-11-06 (planning interview, following moq's [MoQ on rind](https://github.com/moq-dev/moq/blob/main/quest/m1/rind/README.md)):

- rind only: one provider, no exclusion gate. rind's MIGRATE.md advises libraries to keep their old backend as the default. The maintainer chose rind only anyway, and planned rind support for every target ring or aws-lc-rs builds (except wasm) in rind's own tree instead.
- Switch now, without waiting on rind's new targets. Every target this repo releases (Linux, macOS, Windows, iOS and Android on x86_64/aarch64/armv7) is one rind supports today.
- A CPU rind refuses is an error at a builder's finishing method (a new `UnsupportedCpu` variant), not a panic. `new()` stays infallible. A rustls provider the process already installed is still used first, but the CPU check runs before any rind primitive the crate uses directly.
- One child per crate. Each removes its own crate's features and updates its README, feature docs and CPU requirements inline. There is no separate docs page.
- iroh-relay's `server` feature still enables ring. web-transport-iroh doesn't use it.

The epic's own work, once every child has merged:

- A CI guard that no release target's normal or build graph holds aws-lc-sys, ring, or `cc` through a crypto crate. Quiche's and QMux's graphs are exempt.
- Remove leftover aws-lc-rs and ring from the workspace, except QMux's and dev-dependencies that only tests use.

Decision prompts (✅ chosen):

- Goal: ✅ rind only, like moq / rind default, others opt-in / rind as opt-in feature.
- Library policy, given MIGRATE.md: ✅ rind only, and plan rind support for every target ring or aws-lc-rs builds, minus wasm, in kixel-ai/rind / rind default / opt-in rind.
- Crates: ✅ every crate / only qmux / all but iroh.
- Order: wait for rind targets / ✅ switch now.
- CPU refusal: ✅ error at finish / fallible `new()` / panic.
- Split: ✅ a1 epic, per crate / one quest / a2 epic.
- Docs: ✅ inline only / migration page.
