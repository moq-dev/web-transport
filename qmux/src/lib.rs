mod client;
mod error;
mod frame;
mod protocol;
mod server;
mod session;
mod stream;
mod transport;
mod version;

pub(crate) use frame::*;
#[cfg(feature = "websocket")]
pub(crate) use protocol::validate_protocol;
pub use version::*;

pub use client::*;
pub use error::Error;
pub use server::*;
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
