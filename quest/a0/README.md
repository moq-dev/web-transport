# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - close the connection and release the capsule reader
- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - release retained stats descriptors
- [WebSocket handshake timeout](/quest/a0/qmux-ws-handshake-timeout.md) - close sessions whose peer never sends parameters
- [QMux initial stream-count bound](/quest/a0/qmux-max-streams-param.md) - refuse initial_max_streams parameters above 2^60
- [QMux stream-state checks](/quest/a0/qmux-stream-state.md) - refuse MAX_STREAM_DATA and STOP_SENDING for unopened local streams
