//! WebTransport wrapper for WebAssembly.
//!
//! This crate wraps the WebTransport API and provides ergonomic Rust bindings.
//! Some liberties have been taken to make the API more Rust-like and closer to native.

mod client;
mod error;
mod recv;
mod send;
mod session;

pub use client::*;
pub use error::*;
pub use recv::*;
pub use send::*;
pub use session::*;
