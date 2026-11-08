[![crates.io](https://img.shields.io/crates/v/web-transport-wasm)](https://crates.io/crates/web-transport-wasm)
[![docs.rs](https://img.shields.io/docsrs/web-transport-wasm)](https://docs.rs/web-transport-wasm)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-wasm
A wrapper around the WebTransport browser API.

## Poll and async

The browser API is a set of promises, but the surface here is a state machine. Every operation is a
`poll_*` method that a caller steps from its own loop, with the `async` methods as thin wrappers over
them. That is what implements [`web-transport-trait`](https://docs.rs/web-transport-trait)'s `poll`
module, so a sans-I/O caller needs no adapter, and it is what makes an abandoned operation safe: the
promise stays subscribed, so a chunk or an accepted stream is never dropped on the floor between
polls.

Clone the `Session` to run operations concurrently. Each clone keeps its own in-flight operation,
while the underlying browser stream locks are shared — a browser stream can only be locked once.

## Testing

Pull requests run the harness in headless Chromium on Linux through Playwright. Every result row
must pass, including clone/cancellation, stream-opening cleanup, peer reset, datagram and close
checks. Missing browser dependencies, an unavailable peer, page errors and timeouts fail the run.

Controlled stream-open fixtures preserve errors from synchronous exceptions and rejected promises
for both stream directions, including opening again after an error.

Controlled browser `WritableStream` fixtures additionally exercise datagram writers whose close
step is still pending, already-closed writers, and dropped send-task ownership without network
timing. They model the Streams contract; clean closure has not been reproduced through a real
WebTransport session, whose teardown currently exercises the errored-writer path in Chromium.

From the workspace root:

```sh
nix develop
bun install --frozen-lockfile
bunx playwright install --with-deps chromium
just test-browser
```

The browser installation command installs Linux system dependencies through the host package
manager; install those separately when using NixOS. The harness dev-dependency and Nix shell pin
the wasm-bindgen crate/CLI schema. The repository Cargo configuration supplies the unstable
web-sys flags. The build
honors `CARGO_TARGET_DIR`. Playwright reports, traces, screenshots and peer logs are written under
that target directory's `browser-results` folder; CI uploads them on failure. No browser coverage
is silently skipped and failed checks are not retried.

For interactive inspection, `just harness` builds the same page and QUIC peer, then serves it at
`http://localhost:8080` (`just harness 8081` selects another HTTP port). Both commands generate a
short-lived localhost certificate with `dev/setup` when needed. The QUIC peer binds UDP port 4443;
stop other harness instances first. The automated runner waits for peer and HTTP readiness and
bounds startup, test execution and process cleanup.

## Requirements

`web-sys` still gates the WebTransport bindings behind `--cfg=web_sys_unstable_apis`, so this crate
cannot compile without it. The flag can't be enabled by a dependency; it has to come from the final
build. Add it to `.cargo/config.toml`:

```toml
[build]
rustflags = ["--cfg=web_sys_unstable_apis"]
rustdocflags = ["--cfg=web_sys_unstable_apis"]
```

`rustdocflags` is separate because rustdoc does not inherit `rustflags`; without it `cargo doc` and
doctests fail even though `cargo build` succeeds.

Or set `RUSTFLAGS="--cfg=web_sys_unstable_apis"` and `RUSTDOCFLAGS="--cfg=web_sys_unstable_apis"` in
the environment. Both override `.cargo/config.toml` rather than adding to it, so use one or the
other.

Building without the flag fails with a `compile_error!` pointing back here.
