# web-transport-ffi

Low-level UniFFI bindings for WebTransport over HTTP/3 (QUIC). Generates Python, Swift, and Kotlin bindings backed by [`web-transport-quinn`](../web-transport-quinn).

For an ergonomic Python API (async context managers, legacy exception hierarchy), see [`web-transport-rs`](https://pypi.org/project/web-transport-rs/) which wraps this package.

## Session lifetime

Keep the Session object alive while using its streams. Streams do not retain the Session: releasing the final Session reference closes the connection, even if stream objects remain. A helper that returns streams must also return or retain their Session. An explicit close already underway keeps its bounded graceful close after the Session is released; awaiting closure lets it finish, but timeout or cancellation can prevent capsule delivery.

## Building

```bash
# Generate language bindings for the current target:
./build.sh --bindings-only

# Cross-compile native libs for a specific target:
./build.sh --target aarch64-apple-ios --version 0.1.0
```

Artifacts land in `dist/`.
