# [S] Python browser interop's server-close test fails the handshake

## Goal

`test_server_close_all_connections` in
`py/web-transport/tests/browser_interop/test_error_propagation.py` passes every
run. It fails about one CI run in 50 with `WebTransportError: Opening handshake
failed.` from the browser's `transport.ready`: on
[moq-dev/web-transport#563](https://github.com/moq-dev/web-transport/pull/563),
a quest-only change, and on the release PR's run 44079826227 on 2026-11-17.

## Plan

Find whether this is a real bug or the test racing. The server calls
`server.close()` right after `request.accept()` returns. `accept()` returns once
quinn's `Request::ok()` (`rs/web-transport-quinn/src/connect.rs`) has written
the 200 to the CONNECT stream, not once the peer has it, and a QUIC close
discards stream data not yet sent. So the browser may never see the 200 and
reports a failed handshake. Decide whether `server.close()` should be graceful,
flushing pending responses before CONNECTION_CLOSE as a server's
close-everything call reasonably implies, or whether the test should first
wait for the browser's `transport.ready`, without a sleep or retry. Record the
choice and its reason in the PR.

`py/web-transport/test-wheel.sh` runs the whole suite and takes no test
selector, so add one, such as pytest arguments passed through, to repeat just
this test in a loop until it fails.
