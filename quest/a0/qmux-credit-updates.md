# [M] Serialize QMux credit updates and accept stale limits

## Goal

Concurrent receive consumption and stream retirement cannot close a QMux session through reordered credit updates. Valid non-increasing peer limits are ignored while actual flow-control and encoding violations remain errors.

## Plan

Reproduce #439 deterministically: `Credit::consume` assigns an absolute maximum under its mutex, but callers enqueue the corresponding frame after releasing the lock, allowing concurrent MAX_DATA or MAX_STREAMS updates to reverse. Serialize calculation and nonblocking control-channel enqueueing under the existing credit lock at every consuming call site, including reset accounting and stream retirement. Prefer this bounded local change over moving coalescing state into the writer; wake waiters after releasing the lock and review channel/lock ordering for reentrancy and deadlocks.

Ignore non-increasing received MAX_DATA, MAX_STREAM_DATA and both MAX_STREAMS limits without reducing effective credit or spuriously waking blocked operations. This follows [RFC 9000 section 4.1](https://www.rfc-editor.org/rfc/rfc9000.html#section-4.1) and [section 19.11](https://www.rfc-editor.org/rfc/rfc9000.html#section-19.11), whose frame semantics [QMux draft-02 section 4](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html#section-4) preserves. Keep initial transport-parameter validation separate where needed. Preserve existing validation and apply protocol-required frame bounds and stream-state checks before ignoring stale values; genuine flow-control violations must remain errors. Audit every supported Rust QMux version and the TypeScript peer for corresponding behavior; fix affected equivalents within this credit-update scope.

Add controlled-interleaving regressions for concurrent stream-count and connection-data replenishment, plus lower/equal/higher received limits and preserved rejection of malformed input. Establish that regressions fail without the fix. Exercise uni/bidi and supported QMux negotiation modes, with transport integration coverage showing sustained progress. Measure the enqueue-under-lock change across independent session and stream/task counts; record contention/throughput results. Run focused Rust/TypeScript checks as applicable and the repository gates.

Coordinate overlapping receive-credit changes with PR #412. The retained-Session receive hang reported in #439's comment belongs to PR #437; verify its coverage and track its status when resolving the issue, without duplicating that fix or making credit work depend on it unnecessarily. The existing socket-stats teardown quest owns descriptor retention.

Keep public APIs and wire encodings unchanged; accepted peer input changes to match the protocol. Update affected docs/examples inline, with no separate guide. Work on main only: no QMux 0.5 backport, version bump or publication. Prioritize after the session-ownership quests.

## Closes

- [#439](https://github.com/moq-dev/web-transport/issues/439) - fix credit-ordering failures and account for the separately owned receive-hang report

## Related

- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - descriptor retention is a separate resource-lifetime problem
