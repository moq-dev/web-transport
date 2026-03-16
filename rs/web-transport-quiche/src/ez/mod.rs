//! Easy-to-use QUIC connection and stream management.
//!
//! This module provides a simplified interface for working with raw QUIC connections
//! using the quiche implementation. It handles the low-level details of connection
//! management, stream creation, and I/O operations.

mod client;
mod connection;
mod driver;
mod lock;
mod recv;
mod send;
mod server;
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
pub use tls::{CertResolver, CertifiedKey};
pub use tokio_quiche::metrics::{DefaultMetrics, Metrics};
pub use tokio_quiche::settings::QuicSettings as Settings;
