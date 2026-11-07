# [M] Bind native clients by the server's address family

## Goal

Native clients' default sockets match the server's address family, so a macOS dual-stack socket that shares an ephemeral port with another process's IPv4 socket can no longer swallow replies and hang `connect()` until the idle timeout. This covers the Quinn and Noq ClientBuilders, the FFI client and the Quiche ez client. Caller-supplied endpoints and sockets are unchanged.

## Plan

Decided with the maintainer on 2026-11-06 (planning interview):

- Every native default, not only the router's Quinn path. A bind option alone would leave the default broken.
- Quinn and Noq `ClientBuilder` clients (and FFI, which builds its own Quinn endpoint) hold an IPv4 `0.0.0.0:0` endpoint and an IPv6-only `[::]:0` endpoint (`IPV6_V6ONLY`). Each is bound lazily on the first connect to that family and shared by later sessions and clones. Bind failures become a `ClientError` from `connect()`, replacing today's `unwrap()` in `build()`. Hosts with IPv6 disabled fail only IPv6 connects. An IPv4-mapped remote (`::ffff:a.b.c.d`) uses the IPv4 endpoint and its unmapped address.
- `close`, `wait_closed` and `Drop` (in FFI and elsewhere) cover every endpoint bound so far. A connect after `close` fails rather than binding a new endpoint, and so does one still in progress: the bind and the closed check happen under the same lock, so no endpoint is ever bound after `close`.
- `Client::new(endpoint, config)` keeps using the single endpoint it is given.
- The Quiche ez client already binds once per connect. Without `with_bind`, it binds after DNS resolution, using the chosen remote's family.
- No new public bind API. Keep it private until a consumer needs it.
- Regression tests assert the bound family per remote (IPv4 server: IPv4 socket; IPv6 server: v6-only socket) and an IPv4-mapped remote. They fail on today's dual-stack bind. The macOS collision itself is the reporter's repro and isn't run in CI.

Decision prompts (✅ chosen):

- Scope: ✅ all native defaults / Quinn router path only / bind option only / upstream, won't fix.
- Strategy: ✅ lazy per-family / eager both / endpoint per connect.
- Placement: ✅ a1 as one quest / a0 / split per backend.

## Closes

- [#455](https://github.com/moq-dev/web-transport/issues/455) - the implementation completes this report

## Related

- [Quinn on rind](/quest/a1/rind/quinn.md) - rebuilds the same builders' crypto
