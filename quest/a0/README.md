# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Receive-buffer safety](/quest/a0/receive-buffer-safety.md) - remove unsafe exposure in safe defaults
- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [WASM datagram closure](/quest/a0/wasm-datagram-closure.md) - terminate cleanly closed writers
- [Browser regression CI](/quest/a0/browser-ci.md) - execute Chromium harness
- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - release retained stats descriptors
