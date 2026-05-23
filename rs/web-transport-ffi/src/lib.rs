//! UniFFI bindings for [`web_transport_quinn`].
//!
//! Exposes a WebTransport client/server API to Swift, Kotlin, and Python via
//! UniFFI. The shape mirrors `web-transport-quinn` with `Mutex<Option<T>>`
//! take-out patterns for `finish()`/`reset()` and a single shared tokio
//! runtime ([`ffi::RUNTIME`]).

pub mod client;
pub mod error;
mod ffi;
pub mod recv_stream;
pub mod send_stream;
pub mod server;
pub mod session;

uniffi::setup_scaffolding!("web_transport");

#[cfg(test)]
mod test;
