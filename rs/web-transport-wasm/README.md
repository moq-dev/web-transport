[![crates.io](https://img.shields.io/crates/v/web-transport-wasm)](https://crates.io/crates/web-transport-wasm)
[![docs.rs](https://img.shields.io/docsrs/web-transport-wasm)](https://docs.rs/web-transport-wasm)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-wasm
A wrapper around the WebTransport browser API.

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
