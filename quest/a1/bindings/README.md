# Runtime coverage for language bindings

## Goal

Python, Node, Kotlin/JVM and Swift packages have independently maintained PR runtime coverage on the approved host platforms.

## Plan

The children own their language jobs. This epic owns the final coverage map and shared-source trigger audit, checking that Rust/FFI changes trigger every affected job and local testing instructions point to real commands. Do not expand to mobile emulators or a cross-product platform matrix.

## Required

- [Python runtime CI](/quest/a1/bindings/python.md) - exercise the built package on Linux
- [Node runtime CI](/quest/a1/bindings/node.md) - exercise the built package on Linux
- [Kotlin/JVM runtime CI](/quest/a1/bindings/kotlin.md) - exercise the built package on Linux
- [Swift runtime CI](/quest/a1/bindings/swift.md) - exercise the built package on macOS
