# [M] Release socket-stat handles at transport teardown

## Goal

QMux transport termination releases its duplicated stats descriptor even when the application retains a Session clone.

## Plan

`TcpStats` duplicates the socket descriptor and `Session` currently retains it independently of the reader/writer tasks. Move that ownership into shared terminal-state cleanup so normal close, peer failure and blocked-writer termination release it exactly once without racing stats reads.

Approved behavior: socket-derived metrics become unavailable after teardown; other retained session counters remain available. Do not retain a final socket snapshot. Cover TCP and upgraded WebSocket paths, and verify peer-observable EOF or descriptor release while holding a Session clone. Preserve stats while the transport is active. Check PRs #412 and #413 before editing overlapping close paths; do not automatically backport or publish.

## Closes

- [#383](https://github.com/moq-dev/web-transport/issues/383) - the implementation completes this report
