# [S] Keep app-limited samples from lowering QMux's send-rate estimate

## Goal

On Linux, `TcpStats::estimated_send_rate` reports available bandwidth rather than the sender's own rate. An app-limited `delivery_rate` sample can raise the estimate but never lower it, so a live publisher's PROBE estimate stops alternating between its bitrate and path capacity over WebSocket/TCP. The fix ships on main and on the `qmux-v0.5.x` branch that published moq-relay depends on.

## Plan

Decided with the maintainer on 2026-11-06 (planning interview):

- Fix it in qmux, not in the player. The trait documents "estimated available send bandwidth", and a raw app-limited delivery rate breaks that promise.
- Port moq's in-tree `RateFilter` from [moq-dev/moq#6925](https://github.com/moq-dev/moq/pull/6925) so the two qmux copies don't drift. It is a BBR-style windowed maximum over 10 s. A sample that isn't app-limited replaces older maxima once they leave the window. An app-limited sample counts only when it is at least the current maximum. A rejected sample changes nothing, expiry included. A zero rate is no sample. Bit 0 of `tcpi_app_limited` is the flag.
- `TcpStats` gains a small lock around the filter. macOS still reports no rate.
- Test the filter with controlled time: app-limited samples never lower the estimate, a real sample replaces expired maxima, and rejected samples retire nothing.
- Land on main, then backport to `qmux-v0.5.x` in a second PR, as [#412](https://github.com/moq-dev/web-transport/pull/412)/[#413](https://github.com/moq-dev/web-transport/pull/413) do. Published moq-relay (0.17.14) depends on `qmux ^0.5.3`. Don't publish the release yourself.
- Other backends are audited separately in [the quiche audit](/quest/a2/quiche-delivery-rate.md). Quinn and Noq report the pacing rate or cwnd/rtt, which don't have this flaw.

Decision prompts (✅ chosen):

- Goal: fix in qmux / ✅ also audit other backends / won't fix here.
- App-limited handling: hold raise-only (first pick) / windowed max / None when app-limited / `tcpi_pacing_rate`. Replaced by: ✅ port moq's RateFilter / keep hold.
- Quiche audit: ✅ separate quest / same quest.
- 0.5 line: ✅ main + 0.5 backport / main only.
- Placement: ✅ top of a1.

## Closes

- [#460](https://github.com/moq-dev/web-transport/issues/460) - the main-branch fix completes this report

## Related

- [QMux socket teardown](/quest/a0/qmux-socket-teardown.md) - edits the same `TcpStats` ownership
