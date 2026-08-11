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

## Requirements

`web-sys` still gates the WebTransport bindings behind `--cfg=web_sys_unstable_apis`, so this crate
cannot compile without it. The flag can't be enabled by a dependency; it has to come from the final
build. Add it to `.cargo/config.toml`:

```toml
[build]
rustflags = ["--cfg=web_sys_unstable_apis"]
```

Or set `RUSTFLAGS="--cfg=web_sys_unstable_apis"` in the environment. `RUSTFLAGS` overrides
`.cargo/config.toml` rather than adding to it, so use one or the other.

Building without the flag fails with a `compile_error!` pointing back here.
