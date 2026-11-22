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
`server.close()` right after `request.accept()` returns. If `accept()` returns
before the CONNECT's 200 response reaches the socket, close tears the
connection down first and the browser never sees an established session.
That would be a backend bug: an accepted session must be established for the
peer before a later close. Otherwise, the test closes before the browser has
handled the response it already sent, and it should wait for the browser's
`ready`, without a sleep or retry. Reproduce by running the test in a loop
under `just` or the `test-py.yml` steps until it fails.
