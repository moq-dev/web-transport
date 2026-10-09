# [M] Test the Python package at runtime in CI

## Goal

Pull requests exercise the built Python binding on Linux, including loading and basic WebTransport lifecycle behavior.

## Plan

Build and install the `web-transport-rs` wheel in an isolated environment, then run existing unit/integration tests and Chromium browser interop coverage. Verify imports come from the installed wheel rather than a source checkout. Correct the stale package name, PyO3 description and removed stub-file references in the Python README.

Wire the focused command into PR CI with useful failure output and cleanup. Any path filters must cover the shared Rust implementation, generation/build scripts and relevant workflow files. Preserve release packaging; do not require publishing to test a package. Document the local command alongside the binding. Keep this independently shippable from the other language jobs.
