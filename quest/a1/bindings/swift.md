# [M] Test the Swift package at runtime in CI

## Goal

Pull requests exercise the built Swift binding on macOS, including loading and basic WebTransport lifecycle behavior.

## Plan

Generate bindings and build the host artifact, then run the existing Swift package smoke tests and add stream/datagram/close coverage where absent. Verify the tested module uses the generated native library. iOS simulators and the full XCFramework platform matrix are outside this initial scope.

Wire the focused command into PR CI with useful failure output and cleanup. Any path filters must cover the shared Rust implementation, generation/build scripts and relevant workflow files. Preserve release packaging; do not require publishing to test a package. Document the local command alongside the binding. Keep this independently shippable from the other language jobs.
