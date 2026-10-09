# [M] Terminate sends on clean datagram-writer closure

## Goal

A cleanly closed browser datagram writer terminates pending sends, while concurrent senders on an open writer continue making progress.

## Plan

In `rs/web-transport-wasm/src/session.rs::poll_send_datagram`, a clean writable closure can fulfill `ready` with zero desired size, producing repeated wakes. Observe terminal closure independently from readiness; never park an open-writer capacity-race loser exclusively on `closed()`.

Use a controlled spec-compliant writable to reproduce clean closure, since Chromium session teardown currently exercises the errored path. Keep the existing concurrent-sender and errored-writer harness cases. Treat this as hardening unless reachability through actual WebTransport is demonstrated; do not overstate a production reproduction. Update the misleading ready/closure comment with the fix.

## Closes

- [#375](https://github.com/moq-dev/web-transport/issues/375) - the implementation completes this report

## Related

- [Browser CI](/quest/a0/browser-ci.md) - run these regressions automatically; neither quest blocks the other
