# [M] Release qmux 0.5.4 with the receive-credit fix

## Goal

`qmux` 0.5.4 is on crates.io and tagged `qmux-v0.5.4`. It carries
[#520](https://github.com/moq-dev/web-transport/pull/520)'s fix, ported to the
`qmux-v0.5.x` line: stopping or dropping a `RecvStream` with unread data returns
that data's connection credit (`MAX_DATA`), as do late STREAM data after
STOP_SENDING and the RESET_STREAM final-size gap. Consumers pinned to the 0.5
line, such as moq-relay 0.17.x over WebSocket, no longer wedge a connection once
about one window (1 MiB by default) of abandoned stream data has piled up.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan):

- ✅ Port #520 only (rejected: also revive
  [#413](https://github.com/moq-dev/web-transport/pull/413)'s APPLICATION_CLOSE
  flush). That bug never landed on main either and did not cause this stall; a
  maintenance line takes the smallest change.
- ✅ The release PR audits its full diff since `qmux-v0.5.3` before anything is
  published, and records the audit in its body: public API and wire impact
  (none expected), behavior changes a caller could notice, and correctness. A
  finding that blocks the release goes to the maintainer instead of being
  published.
- ✅ An agent publishes after the maintainer merges (rejected: the maintainer
  publishes by hand). Merging the audited PR is the approval. Then run
  `cargo publish -p qmux` from the merged head, tag that commit with
  `git tag qmux-v0.5.4 <merged-head>` (`cargo publish` makes no tag), and push
  it with `git push origin refs/tags/qmux-v0.5.4`, as for 0.5.3
  ([#461](https://github.com/moq-dev/web-transport/pull/461)). This host's
  crates.io credentials allow it.

The 0.5 line is about 3.7k lines away from main, so this is a port, not a
cherry-pick. Keep 0.5's `Writer::send` contract and public API. Keep #520's
choices: stopped entries stay until FIN or RESET_STREAM, so a peer that ignores
STOP_SENDING stays bounded by its stream limit, and the legacy `webtransport`
binding still retires on STOP_SENDING.

One PR, based on `qmux-v0.5.x`, holds the port, the 0.5.4 version bump, and the
CHANGELOG entry. That branch has no release-plz and no `quest/`, so once 0.5.4
is published, delete this quest in a main PR.
[@moq/qmux 0.3.5](/quest/a0/qmux-js-release.md) can carry the deletion if it
lands later.

Validation: run the stall on the real crate before and after. Over TCP on
localhost, a sender opens 16 KiB uni streams and the receiver accepts each and
drops it unread. 0.5.3 blocks after exactly 64 streams (1,048,576 bytes), and
68 with 1 KiB read per stream; 0.5.4 passes 400. Also port #520's regression
tests.

Caused by: [#199](https://github.com/moq-dev/web-transport/pull/199) added QMux
connection flow control and credited only bytes the application reads.

## Related

- [@moq/qmux 0.3.5](/quest/a0/qmux-js-release.md) - the same fix for the
  browser side
- [#507](https://github.com/moq-dev/web-transport/issues/507) - the leak, fixed
  on main by #520
