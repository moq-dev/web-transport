# [S] Fix the flaky Python certificate-reload test

## Goal

`test_reload_existing_session_survives` in `py/web-transport/tests/integration/test_server.py` passes deterministically. It failed once in the `wheel` job of [#517](https://github.com/moq-dev/web-transport/pull/517) with `SessionClosedByPeer: code 0` and passed 5/5 when rerun alone.

## Plan

Reproduce it in a loop before changing anything. The suspected mechanism is a race in the test: the server task returns after writing the echo and its `session` is dropped, closing the session with code 0 before the client has read the echo. Confirm or replace that explanation, then fix the root cause at the lowest responsible layer: the test's lifetime handling if the test is wrong, or the binding if dropping a session discards data the peer was already sent. No retries, sleeps or wider timeouts. Check the neighboring Python tests that use the same server-task pattern for the same race.
