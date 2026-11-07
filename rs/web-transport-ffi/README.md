# web-transport-ffi

Low-level UniFFI bindings for WebTransport over HTTP/3 (QUIC). Generates Python, Swift, and Kotlin bindings backed by [`web-transport-quinn`](../web-transport-quinn).

For an ergonomic Python API (async context managers, legacy exception hierarchy), see [`web-transport-rs`](https://pypi.org/project/web-transport-rs/) which wraps this package.

## Building

```bash
# Generate language bindings for the current target:
./build.sh --bindings-only

# Cross-compile native libs for a specific target:
./build.sh --target aarch64-apple-ios --version 0.1.0
```

Artifacts land in `dist/`.


## Rejection diagnostics

`WebTransportError::SessionRejected` preserves `status_code`, `protocol`, and
`headers`, a sequence of `HttpHeader { name, value }` field lines. Names are
lowercase and values are bytes; repeated names retain all values. `detail`
contains the diagnostic message without dumping credentials from the headers.
This extends the exported error record and changes the UniFFI ABI: regenerate
Python, Kotlin, and Swift bindings together with the native library. Existing
Rust matches should include `..` or bind the new fields.

`SessionRequest.reject(status_code = 404, headers = [], protocol = None)` sends
the supplied repeated `HttpHeader` lines through the native rejection path and
waits for delivery or peer termination. The optional protocol must have been
offered by the peer. Managed generic fields, malformed values and oversized
responses are rejected before consuming the request. Default arguments preserve
existing foreign-language calls; direct Rust callers must supply the new fields.
