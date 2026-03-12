//! QMux protocol (draft-ietf-quic-qmux-00) over reliable transports.
//!
//! Provides QUIC-style multiplexed streams over TCP, TLS, and WebSocket,
//! with backwards compatibility for the legacy `webtransport` wire format.

mod error;
mod protocol;
pub mod proto;
mod session;
mod stream;
mod transport;

/// Plain TCP transport.
#[cfg(feature = "tcp")]
pub mod tcp;

/// TLS over TCP transport.
#[cfg(feature = "tls")]
pub mod tls;

/// WebSocket transport.
#[cfg(feature = "ws")]
pub mod ws;

use proto::*;

pub use error::Error;
pub use transport::Transport;
pub use session::{RecvStream, SendStream, Session};
pub use stream::{StreamDir, StreamId};

/// All supported ALPN identifiers, in preference order.
///
/// Use this when configuring TLS to advertise QMux support.
/// New versions are added automatically; callers don't need to update.
pub const ALPNS: &[&str] = &[ALPN_QMUX, ALPN_WEBTRANSPORT];

/// All supported WebSocket subprotocol prefixes, in preference order.
///
/// Each prefix is prepended to the application protocol name during
/// WebSocket subprotocol negotiation (e.g. `"qmux-00." + "moq-03"`).
pub const PREFIXES: &[&str] = &[PREFIX_QMUX, PREFIX_WEBTRANSPORT];

const ALPN_WEBTRANSPORT: &str = "webtransport";
const ALPN_QMUX: &str = "qmux-00";
const PREFIX_WEBTRANSPORT: &str = "webtransport.";
const PREFIX_QMUX: &str = "qmux-00.";
