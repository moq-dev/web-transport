use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::build_stream_session;
use crate::{Error, Protocol, Session, Version};

/// Builder for a QMux session over plain TCP.
///
/// TCP has no ALPN, so the wire-format `version` is given explicitly and any
/// application protocol is negotiated in-band via [`Config::protocols`].
///
/// ```no_run
/// # async fn f(addr: std::net::SocketAddr) -> Result<(), qmux::Error> {
/// use qmux::{tcp, Version};
/// use web_transport_trait::Session as _;
///
/// // Negotiate one of these protocols (server preference wins):
/// let session = tcp::Config::new(Version::QMux01)
///     .protocols(["moq-lite-04", "moq-lite-03"])
///     .connect(addr)
///     .await?;
/// let agreed = session.protocol(); // e.g. Some("moq-lite-04")
/// # let _ = agreed; Ok(())
/// # }
/// ```
///
/// For flow-control tuning beyond version + protocols, build a [`crate::Config`]
/// and drive [`crate::Stream`] + [`Session::connect`] yourself.
#[derive(Debug, Clone)]
pub struct Config {
    inner: crate::Config,
}

impl Config {
    /// Start building a TCP session speaking QMux `version`.
    pub fn new(version: Version) -> Self {
        Self {
            inner: crate::Config::new(version),
        }
    }

    /// Advertise these application protocols for in-band negotiation, in
    /// preference order. Omit to run without an application protocol.
    pub fn protocols<I, S>(mut self, protocols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.inner.protocol = Protocol::Negotiate(protocols.into_iter().map(Into::into).collect());
        self
    }

    /// Override how long establishment waits for the peer's transport
    /// parameters before failing with [`Error::HandshakeTimeout`]. Defaults to
    /// 10s; a zero duration waits indefinitely. See
    /// [`Config::handshake_timeout`](crate::Config::handshake_timeout).
    pub fn handshake_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.inner.handshake_timeout = timeout;
        self
    }

    /// Connect to `addr` and start a client session.
    ///
    /// Awaits the peer's transport parameters before returning, so
    /// [`Session::protocol`](web_transport_trait::Session::protocol) is populated
    /// on the returned session (bounded by
    /// [`handshake_timeout`](Self::handshake_timeout)).
    pub async fn connect(self, addr: impl ToSocketAddrs) -> Result<Session, Error> {
        let stream = TcpStream::connect(addr).await?;
        finish(stream, self.inner, false).await
    }

    /// Start a server session over an accepted TCP stream.
    ///
    /// This awaits the QMux handshake (the peer's transport parameters), bounded
    /// by [`handshake_timeout`](Self::handshake_timeout). Drive each connection
    /// with `tokio::spawn` so a slow or non-cooperative peer can't stall your
    /// `listener.accept()` loop.
    pub async fn accept(self, stream: TcpStream) -> Result<Session, Error> {
        finish(stream, self.inner, true).await
    }
}

async fn finish(
    stream: TcpStream,
    config: crate::Config,
    is_server: bool,
) -> Result<Session, Error> {
    // `build_stream_session` awaits the peer's transport parameters before
    // returning, so `protocol()` is resolved on the session we hand back
    // (bounded by the config's handshake timeout).
    build_stream_session(stream, config, is_server).await
}
