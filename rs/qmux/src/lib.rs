//! QMux protocol (draft-ietf-quic-qmux-01) over reliable transports.
//!
//! Provides QUIC-style multiplexed streams over TCP, TLS, and WebSocket,
//! with backwards compatibility for the legacy `webtransport` wire format.

// ALPN/subprotocol negotiation is only used by the TLS and WebSocket transports.
#[cfg(any(feature = "tls", feature = "ws"))]
mod alpn;
mod config;
mod credit;
mod error;
pub mod proto;
mod protocol;
mod sched;
mod session;
mod stream;

/// Transport abstraction and the byte-stream [`transport::Stream`] implementation.
pub mod transport;

/// Plain TCP transport.
#[cfg(feature = "tcp")]
pub mod tcp;

/// Unix domain socket transport.
#[cfg(all(unix, feature = "uds"))]
pub mod uds;

/// TLS over TCP transport.
#[cfg(feature = "tls")]
pub mod tls;

/// WebSocket transport.
#[cfg(feature = "ws")]
pub mod ws;

#[cfg(feature = "ws")]
pub use tokio_tungstenite;

#[cfg(feature = "ws")]
pub use tokio_tungstenite::tungstenite;

#[cfg(feature = "ws")]
pub use ws::{Client, KeepAlive, Server};

use proto::*;

pub use config::{Config, Protocol};
pub use error::Error;
pub use proto::Version;
pub use session::{RecvStream, SendStream, Session};
pub use stream::{StreamDir, StreamId};
pub use transport::{Transport, TransportReader, TransportWriter};
// The concrete byte-stream transport lives at `transport::Stream` rather than the
// crate root, so the name doesn't collide with the STREAM-frame `Stream` type.

/// All supported ALPN identifiers, in preference order.
///
/// Use this when configuring TLS to advertise QMux support.
/// For version-specific ALPNs, use [`Version::alpn()`].
pub const ALPNS: &[&str] = &["qmux-01", "qmux-00", "webtransport"];
