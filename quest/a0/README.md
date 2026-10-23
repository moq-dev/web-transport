# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Bound decoded CONNECT headers](/quest/a0/qpack-field-section.md) - a CONNECT can make its receiver hold at most a 16 KiB field section, released as web-transport-proto 0.6.3
- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - close the connection and release the capsule reader
- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - release retained stats descriptors
- [WebSocket handshake timeout](/quest/a0/qmux-ws-handshake-timeout.md) - close sessions whose peer never sends parameters
