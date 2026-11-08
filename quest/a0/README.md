# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - close the connection and release the capsule reader
- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - release retained stats descriptors
- [QMux stream-state checks](/quest/a0/qmux-stream-state.md) - refuse MAX_STREAM_DATA and STOP_SENDING for unopened local streams
- [Node session and reader lifetime](/quest/a0/node-session-lifetime.md) - `closed` rejects on an abnormal end, and an abandoned reader releases its stream
