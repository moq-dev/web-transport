//! QMux protocol (draft-ietf-quic-qmux-02) over reliable transports.
//!
//! Provides QUIC-style multiplexed streams over TCP, TLS, and WebSocket.
//! Speaks draft-02 by default, negotiating down to draft-01 or draft-00, with
//! backwards compatibility for the legacy `webtransport` wire format.
//!
//! # Layout
//!
//! The crate root holds the transport-independent session API: [`Session`] and
//! its [`SendStream`] / [`RecvStream`] halves, plus the [`Config`], [`Version`],
//! and [`Error`] types they share.
//!
//! Everything tied to a specific transport lives in that transport's module, so
//! the generic names stay unambiguous when several are in scope at once:
//!
//! | Module | Entry point | Feature |
//! | --- | --- | --- |
//! | `tcp` | `tcp::Config` | `tcp` |
//! | `uds` | `uds::Config` | `uds` (Unix only) |
//! | `tls` | `tls::Client`, `tls::Server` | `tls` |
//! | `ws` | `ws::Client`, `ws::Server`, `ws::Upgraded` | `ws` |
//! | [`transport`] | [`transport::Stream`] | `tcp` or `uds` |
//!
//! For anything the built-in modules don't cover, wrap an
//! [`AsyncRead`](tokio::io::AsyncRead) + [`AsyncWrite`](tokio::io::AsyncWrite)
//! byte stream in [`transport::Stream`], or implement
//! [`transport::Transport`] yourself, and pass it to [`Session::connect`] /
//! [`Session::accept`].

// ALPN/subprotocol negotiation is only used by the TLS and WebSocket transports.
#[cfg(any(feature = "tls", feature = "ws"))]
mod alpn;
mod config;
mod credit;
mod error;
mod proto;
mod protocol;
mod rtt;
mod sched;
mod session;
mod shared;
mod socket;
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

use proto::*;

pub use config::{Config, Protocol};
pub use error::Error;
pub use proto::Version;
pub use session::{RecvStream, SendStream, Session};
#[cfg(unix)]
pub use socket::TcpStats;
pub use socket::{SharedSocketStats, SocketStats};
pub use stream::{StreamDir, StreamId};
// Transport-specific types are deliberately *not* re-exported here; they stay
// behind their module path (`transport::Transport`, `ws::Client`, `tls::Client`,
// ...) so that names shared across transports don't collide at the crate root.

/// All supported ALPN identifiers, in preference order.
///
/// Use this when configuring TLS to advertise QMux support.
/// For version-specific ALPNs, use [`Version::alpn()`].
pub const ALPNS: &[&str] = &["qmux-02", "qmux-01", "qmux-00", "webtransport"];
