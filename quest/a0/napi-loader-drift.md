# [XS] Stop the Node test build from rewriting its committed loader

## Goal

Running `just test` on a clean checkout leaves it clean. Today the native build regenerates `js/web-transport/napi.cjs` and `napi.d.cts` with output that differs from the committed files, so every agent has to revert them by hand.

## Plan

`bun.lock` already pins `@napi-rs/cli` 3.10.9, so the committed files most likely predate the locked CLI rather than the CLI drifting. Confirm the cause, then make the committed files match what the pinned toolchain generates. Recommended: regenerate and commit them, and have CI fail when `build:native` leaves a diff, so the drift cannot return. Keep the files committed: they are part of the published package.
