# [M] Test the Node package at runtime in CI

## Goal

Pull requests exercise the built Node binding on Linux, including loading and basic WebTransport lifecycle behavior.

## Plan

Build the NAPI artifact and test the packaged JavaScript entry point under Node. Cover connection setup, uni/bi streams, datagrams, closure and process exit so leaked runtime handles fail the test. Use Bun for repository package tooling; this quest specifically tests Node runtime behavior.

Wire the focused command into PR CI with useful failure output and cleanup. Any path filters must cover the shared Rust implementation, generation/build scripts and relevant workflow files. Preserve release packaging; do not require publishing to test a package. Document the local command alongside the binding. Keep this independently shippable from the other language jobs.
