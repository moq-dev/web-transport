# Runtime coverage for language bindings

## Goal

Python, Node, Kotlin/JVM and Swift packages have independently maintained PR runtime coverage on the approved host platforms.

## Plan

The children own their language jobs. This epic owns the final coverage map and shared-source trigger audit, checking that Rust/FFI changes trigger every affected job and local testing instructions point to real commands. Do not expand to mobile emulators or a cross-product platform matrix.

## Required

- [Wheel on Ubuntu 26.04](/quest/a1/bindings/wheel-ubuntu-26.md) - the Python browser job passes on the current runner image
