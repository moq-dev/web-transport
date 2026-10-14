# [S] Bound a WebSocket QMux handshake

## Goal

A QMux session over WebSocket closes with an error when the peer never sends TRANSPORT_PARAMETERS within `Config::handshake_timeout`, for `ws::Client`, `ws::Server` and `ws::Upgraded` alike. Today a peer that completes the WebSocket upgrade and then stays silent holds the session open indefinitely.

## Plan

`Session::connect` and `Session::accept` bound the parameter exchange, but every ws builder starts through `Session::new`, which never waits for the peer's parameters. Arm the deadline inside the session started by `Session::new` and close the session with the same error `Session::connect` returns on timeout if the peer's TRANSPORT_PARAMETERS haven't arrived by then. This is the lowest layer that covers all three builders without an API change. Do not make `ws::Upgraded::connect`/`accept` async; that would break the API. Wire formats that exchange no transport parameters arm no deadline. Fold `Session::connect`/`accept`'s own timeout into the same mechanism if that simplifies things.

Remove the "`handshake_timeout` does not apply" notes from the three `with_session_config` docs. Cover each builder on paused time. Main only; backport to `qmux-v0.5.x` only if moq asks.
