//! Easy-to-use QUIC connection and stream management.
//!
//! This module provides a simplified interface for working with raw QUIC connections
//! using the quiche implementation. It handles the low-level details of connection
//! management, stream creation, and I/O operations.
//!
//! Every operation is available both as an `async` method and as a `poll_*` method. The
//! latter take a [`kio::Waiter`] rather than a [`Waker`](std::task::Waker), because a
//! connection parks many callers on the same state and has no way to deregister them: a
//! waiter's registrations are weak and die with it, so a caller that gives up — a
//! `timeout` that expires, a dropped future — releases its slot. Drive one with
//! [`kio::wait`], or hold a waiter across polls yourself when implementing a
//! `Context`-based `poll_*` on top.

mod client;
mod connection;
mod driver;
mod lock;
mod recv;
mod send;
mod server;
mod socket;
mod stream;
pub mod tls;

pub use client::*;
pub use connection::*;
pub use recv::*;
pub use send::*;
pub use server::*;
pub use stream::*;

use driver::*;
use lock::*;

pub use rustls_pki_types::{CertificateDer, PrivateKeyDer};
pub use tls::{CertResolver, CertifiedKey, ClientAuth};
pub use tokio_quiche::metrics::{DefaultMetrics, Metrics};
/// Compression applied to the qlog traces written to [`Settings::qlog_dir`].
pub use tokio_quiche::settings::QlogCompression;
pub use tokio_quiche::settings::QuicSettings as Settings;
