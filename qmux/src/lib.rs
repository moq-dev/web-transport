mod error;
mod frame;
mod protocol;
mod session;
mod stream;
mod transport;
mod version;

#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "tls")]
pub mod tls;
#[cfg(feature = "ws")]
pub mod ws;

pub(crate) use frame::*;
pub use version::*;

pub use error::Error;
pub use session::{RecvStream, SendStream, Session};
pub use stream::{StreamDir, StreamId};

/// Legacy ALPN identifier for backwards compatibility with web-transport-ws.
pub const ALPN_WEBTRANSPORT: &str = "webtransport";

/// QMux draft-00 ALPN identifier.
pub const ALPN_QMUX: &str = "qmux-00";

/// Legacy WebSocket subprotocol prefix.
pub const PREFIX_WEBTRANSPORT: &str = "webtransport.";

/// QMux WebSocket subprotocol prefix.
pub const PREFIX_QMUX: &str = "qmux-00.";
