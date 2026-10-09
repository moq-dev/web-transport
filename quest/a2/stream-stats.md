# [L] Expose optional async per-stream statistics

## Goal

Applications can asynchronously collect precisely defined stream statistics without treating unsupported values as zero or queue acceptance as delivery.

## Plan

Use the [W3C stream-statistics definitions](https://www.w3.org/TR/webtransport/#webtransportsendstreamstats) as the semantic reference. Use asynchronous collection across backends: browser `getStats()` returns a Promise, while native implementations may complete immediately. This avoids a separate refresh API and stale-cache semantics. Provide send/receive data appropriate to each stream and optional measurements rather than reusing connection-level counters that include overhead.

Count application payload accepted/written and read, covering partial writes, helpers, cancellation and protocol-header exclusion. Define sent bytes as payload transmitted at least once, excluding retransmissions; define acknowledged bytes as the contiguous acknowledged prefix, not peer application consumption. Unsupported sent/ack measurements remain unavailable. Feature-detect browser collection support.

The inspected Quinn/Noq/Quiche APIs expose no live per-stream transmitted/acknowledged count. QMux feeding an underlying writer is not proof of wire transmission or acknowledgment. Do not add misleading substitutes. Implement locally available counters and supported browser metrics, preserving default methods where possible for downstream implementors. Add backend conformance tests and native/WASM compilation. Keep metric semantics and examples in the feature docs. Native upstream access is tracked separately and does not block this initial API.

## Related

- [Issue #368](https://github.com/moq-dev/web-transport/issues/368) - this API is one part of the request; native integration owns closure
- [Native metric integration](/quest/a2/native-stream-stats.md) - expose supported delivery progress after upstream access is available
- [Native metric availability](/quest/a2/native-stats-upstream.md) - external prerequisite for richer native measurements
