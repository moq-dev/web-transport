[![crates.io](https://img.shields.io/crates/v/web-transport-wasm)](https://crates.io/crates/web-transport-wasm)
[![docs.rs](https://img.shields.io/docsrs/web-transport-wasm)](https://docs.rs/web-transport-wasm)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-wasm
A wrapper around the WebTransport browser API.

## Building

If `cargo` is not in `PATH`

```
export PATH=$PATH:/media/user/path/to/rust/.cargo/bin
```

```
RUSTFLAGS="--cfg=web_sys_unstable_apis" \
CARGO_HOME=/media/user/path/to/rust/.cargo \
RUSTUP_HOME=/media/user/path/to/rust/.rustup \
/media/user/path/to/rust/.cargo/bin/wasm-pack build --target web --out-dir pkg
```
