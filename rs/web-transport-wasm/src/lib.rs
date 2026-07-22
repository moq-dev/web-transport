//! WebTransport wrapper for WebAssembly.
//!
//! This crate wraps the WebTransport API and provides ergonomic Rust bindings.
//! Some liberties have been taken to make the API more Rust-like and closer to native.
//!
//! # Requirements
//!
//! `web-sys` still gates the WebTransport bindings behind `--cfg=web_sys_unstable_apis`,
//! so this crate cannot compile without that flag. It cannot be enabled by a dependency;
//! it has to be set by the final build, for example via `.cargo/config.toml`:
//!
//! ```toml
//! [build]
//! rustflags = ["--cfg=web_sys_unstable_apis"]
//! ```

#[cfg(not(web_sys_unstable_apis))]
compile_error!(
    "web-transport-wasm requires `--cfg=web_sys_unstable_apis` because web-sys gates the \
     WebTransport bindings behind it. This flag cannot be enabled by a dependency; add it to \
     your own build, for example in `.cargo/config.toml`:\n\n\
     \x20   [build]\n\
     \x20   rustflags = [\"--cfg=web_sys_unstable_apis\"]\n\n\
     or set `RUSTFLAGS=\"--cfg=web_sys_unstable_apis\"` in the environment. Note that RUSTFLAGS \
     overrides `.cargo/config.toml`, so only one of the two takes effect."
);

// Gated so missing web-sys bindings don't bury the error above.
#[cfg(web_sys_unstable_apis)]
mod client;
#[cfg(web_sys_unstable_apis)]
mod error;
#[cfg(web_sys_unstable_apis)]
mod recv;
#[cfg(web_sys_unstable_apis)]
mod send;
#[cfg(web_sys_unstable_apis)]
mod session;

#[cfg(web_sys_unstable_apis)]
pub use client::*;
#[cfg(web_sys_unstable_apis)]
pub use error::*;
#[cfg(web_sys_unstable_apis)]
pub use recv::*;
#[cfg(web_sys_unstable_apis)]
pub use send::*;
#[cfg(web_sys_unstable_apis)]
pub use session::*;
