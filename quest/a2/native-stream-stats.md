# [M] Connect native delivery progress to stream statistics

## Goal

Quinn and Quiche send streams expose actual supported delivery progress through the async optional-statistics API, completing the native delivered-versus-queued use case in #368.

## Plan

Use public upstream accessors once the prerequisite clears. Map accepted payload, transmitted payload and the contiguous acknowledged prefix to the documented trait semantics. Do not substitute transport capacity, write acceptance, flush completion or a connection-level ACK count for stream delivery. Reset and completion behavior must be explicit.

Verify backpressure builds a visible distinction between accepted and acknowledged data, and that peer acknowledgments advance progress without counting retransmissions twice. Exercise partial writes and protocol-header exclusion. Wire Noq/Iroh where their supported public dependencies provide equivalent accessors; keep unsupported fields unavailable and document the capability matrix. Keep API reference/examples with this change.

Any required dependency release or pin bump is its own prerequisite quest once the concrete version is known. Do not fork transport internals or broaden this into a sans-I/O rewrite. The initial optional API alone does not close the original delivery-progress request.

## Required

- [Async stream-statistics API](/quest/a2/stream-stats.md) - shared semantics and optional async collection
- [Native metric availability](/quest/a2/native-stats-upstream.md) - usable public upstream progress accessors

## Closes

- [#368](https://github.com/moq-dev/web-transport/issues/368) - native delivered-versus-queued progress is available
