# [XS] Release @moq/qmux 0.3.5 with the receive-credit fix

## Goal

`@moq/qmux` 0.3.5 is on npm, carrying
[#516](https://github.com/moq-dev/web-transport/pull/516): cancelling a receive
stream returns its connection credit. Browsers on the WebSocket fallback no
longer stall once a connection window's worth of cancelled group data has piled
up. 0.3.4 lacks the fix. `@moq/net`'s `^0.3.3` range allows 0.3.5, but a
lockfile pinned to 0.3.4 stays there until moq-dev/moq#8780 bumps it.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan): ✅ ship main as 0.3.5 (rejected:
only #516 on a branch from 0.3.4, which needs a new branch and a manual npm
publish this repository doesn't have). A version-bump PR on main publishes
through `release-js.yml` when it merges.

A pre-release audit on 2026-11-14 covered `js/qmux` from 0.3.4's commit
`337787c` through `d0072fd`: #451, #483, #484, #501, #505, #516. It found no
blockers:

- the type declarations are identical to 0.3.4;
- the stricter checks (#483, #484) never trigger on what qmux 0.5.x sends;
- 318 unit tests pass, and interop passes against main's Rust and against 0.5.3.

The release PR audits only what landed in `js/qmux` after `d0072fd`, and
records both audits in its body.

One behavior change to state in the release notes: a cancelled incoming stream
keeps its stream-count slot until the peer sends FIN or RESET_STREAM, as RFC
9000 requires. A peer that leaves a stopped stream idle can therefore exhaust
the incoming limit (100 by default). The audit reproduced this against a Rust
0.5.3 app that holds its streams idle. moq-relay resets its group streams
promptly. Checking the relay's other streams end to end belongs to moq-dev/moq's
release-line bump.

## Related

- [qmux 0.5.4](/quest/a0/qmux-05-recv-credit.md) - the Rust side of the same
  fix
