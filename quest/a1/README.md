# Bindings, configuration and documentation

## Goal

Make supported packages testable, configurable and accurately documented.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Rust crates on rind](/quest/a1/rind/README.md) - no C crypto outside Quiche
- [Binding runtime coverage](/quest/a1/bindings/README.md) - independent language jobs
- [Node idle configuration](/quest/a1/node-idle/README.md) - typed native input and Node options
- [Response headers](/quest/a1/response-headers.md) - preserve success and rejection metadata
- [Rust platform verifier](/quest/a1/platform-verifier.md) - `with_system_roots()` verifies with the OS trust store
- [FFI platform verifier](/quest/a1/ffi-platform-verifier.md) - bindings verify on iOS and Android
- [Package selection](/quest/a1/package-selection.md) - verified comparison and selection guide
