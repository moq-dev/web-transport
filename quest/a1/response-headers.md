# [L] Preserve CONNECT response and rejection headers

## Goal

Applications can encode and inspect ordinary CONNECT response headers, including repeated values, and inspect rejection headers through structured errors.

## Plan

Extend `ConnectResponse` in `rs/web-transport-proto/src/connect.rs` using the request header representation as a starting point. Typed status and negotiated-subprotocol fields remain authoritative; reject or filter conflicting generic entries consistently so encoding cannot emit duplicate reserved fields. Preserve repeated ordinary headers.

Retain decoded response information when a non-success status becomes a structured connection error rather than discarding all but status. Trace protocol consumers through Quinn, Noq, Quiche, Iroh and the router, updating affected errors, wrappers and examples. Browser APIs cannot expose headers the browser does not provide; do not fabricate parity.

Test success/rejection round trips, repeated headers, reserved fields, malformed responses, existing size limits and exact frame consumption. Audit semver compatibility of exported structs and enum variants. API reference and diagnostic examples belong to this change.

## Closes

- [#67](https://github.com/moq-dev/web-transport/issues/67) - the implementation completes this report
