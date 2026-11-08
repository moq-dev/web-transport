# web-transport-ffi

Low-level UniFFI bindings for WebTransport over HTTP/3 (QUIC). Generates Python, Swift, and Kotlin bindings backed by [`web-transport-quinn`](../web-transport-quinn).

For an ergonomic Python API (async context managers, legacy exception hierarchy), see [`web-transport-rs`](https://pypi.org/project/web-transport-rs/) which wraps this package.

Clients lazily share one socket per destination address family: IPv4 uses
`0.0.0.0:0`, IPv6 uses IPv6-only `[::]:0`, and IPv4-mapped IPv6 uses IPv4.
Socket setup errors are returned by `connect`. `close` and final client drop
close both sockets, including pending connections. Call `close` before awaiting
`wait_closed` to prevent new connections while both sockets drain.

## Building

```bash
# Generate language bindings for the current target:
./build.sh --bindings-only

# Cross-compile native libs for a specific target:
./build.sh --target aarch64-apple-ios --version 0.1.0
```

Artifacts land in `dist/`.
