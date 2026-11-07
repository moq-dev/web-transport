# [S] Audit quiche's send-rate estimate for app-limited samples

## Goal

web-transport-quiche's `estimated_send_rate` reports available bandwidth under every congestion controller it uses, or its docs say exactly where it doesn't.

## Plan

`ez/connection.rs` reports quiche's `PathStats::delivery_rate * 8` as-is. In quiche 0.30 that value is BBR2's filtered bandwidth estimate under gcongestion, but the latest raw rate sample under legacy CUBIC/Reno. That is the flaw [#460](https://github.com/moq-dev/web-transport/issues/460) found in qmux.

Confirm which controllers web-transport-quiche configures by default and on request, and whether app-limited samples reach the reported value. If they do, prefer a filtered value quiche exposes publicly (such as `max_bandwidth` where it exists), or apply the same rule as [the qmux filter](https://github.com/moq-dev/web-transport/pull/487) when quiche exposes the app-limited state. Otherwise document the limitation on the method. Use only quiche's public API, and don't fork its recovery code.

Decided with the maintainer on 2026-11-06: a separate quest from the qmux fix, in a2.
