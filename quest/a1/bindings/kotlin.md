# [M] Test the Kotlin/JVM package at runtime in CI

## Goal

Pull requests exercise the built Kotlin/JVM binding on Linux, including loading and basic WebTransport lifecycle behavior.

## Plan

Generate the bindings and native library, build the JVM artifact and exercise a client/server smoke test through that artifact. Test loading from packaged resources. Fix README commands that reference a nonexistent `just kt` module. Android emulators are outside this initial scope.

Wire the focused command into PR CI with useful failure output and cleanup. Any path filters must cover the shared Rust implementation, generation/build scripts and relevant workflow files. Preserve release packaging; do not require publishing to test a package. Document the local command alongside the binding. Keep this independently shippable from the other language jobs.
