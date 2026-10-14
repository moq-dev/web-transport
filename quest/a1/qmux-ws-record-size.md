# [S] Advertise a record size the WebSocket accepts

## Goal

`ws::Client` and `ws::Server` never advertise a `max_record_size` larger than their WebSocket will accept, so a peer that sends a record within the advertised limit can't trip tungstenite's frame limit and kill the session.

## Plan

Each QMux record is one WebSocket message, so tungstenite's `max_frame_size`/`max_message_size` (16 MiB frame by default) caps the usable record size. Give `ws::Server` a `with_config(WebSocketConfig)` matching `ws::Client`'s and pass it to `accept_hdr_async_with_config`. Both builders then advertise `min(max_record_size, the WebSocket's limit)`; the datagram limit already derives from the record size. Clamping keeps the advertised ceiling truthful without failing a connect. The one exception is a WebSocket limit below the QMux minimum (`DEFAULT_MAX_RECORD_SIZE`): the peer rejects that as a TRANSPORT_PARAMETER error, so `connect`/`accept` return an error locally instead of advertising it.

`ws::Upgraded` wraps any tungstenite `Sink`/`Stream`, so qmux can't see its limit; keep its `with_session_config` note telling callers to stay within it. Update the Client and Server docs to describe the clamp. Main only; backport to `qmux-v0.5.x` only if moq asks.
