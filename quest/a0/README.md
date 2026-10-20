# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - close the connection and release the capsule reader
- [Noq close cancellation](/quest/a0/noq-close-cancellation.md) - release sessions when graceful-close futures are cancelled
- [Quiche final Session drop](/quest/a0/quiche-session-drop.md) - keep capsule tasks outside frontend ownership
- [QMux credit updates](/quest/a0/qmux-credit-updates.md) - serialize advertisements and ignore stale peer limits
- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [WASM datagram closure](/quest/a0/wasm-datagram-closure.md) - terminate cleanly closed writers
- [Browser regression CI](/quest/a0/browser-ci.md) - execute Chromium harness
- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - release retained stats descriptors
- [WebSocket handshake timeout](/quest/a0/qmux-ws-handshake-timeout.md) - close sessions whose peer never sends parameters
