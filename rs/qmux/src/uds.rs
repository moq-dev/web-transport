use std::path::Path;

use tokio::net::UnixStream;

use crate::transport::build_stream_session;
use crate::{Error, Protocol, Session, Version};

/// Builder for a QMux session over a Unix domain socket.
///
/// Like [`crate::tcp::Config`], but over a Unix socket — ideal for same-host MoQ
/// where the TLS/ALPN handshake of a network transport would be pure overhead.
/// The application protocol (if any) is negotiated in-band via
/// [`Config::protocols`].
///
/// ```no_run
/// # async fn f(path: &str) -> Result<(), qmux::Error> {
/// use qmux::{uds, Version};
///
/// let session = uds::Config::new(Version::QMux01)
///     .protocols(["moq-lite-04"])
///     .connect(path)
///     .await?;
/// # let _ = session; Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    inner: crate::Config,
}

impl Config {
    /// Start building a Unix-socket session speaking QMux `version`.
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

    /// Advertise a requested resource `path`.
    ///
    /// A Unix socket has no URL, so a client that needs to address a specific
    /// resource sends the path in-band. The peer reads it via
    /// [`Session::path`](crate::Session::path). Omit to send no path.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.inner.path = Some(path.into());
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

    /// Connect to the socket at `path` and start a client session.
    pub async fn connect(self, path: impl AsRef<Path>) -> Result<Session, Error> {
        let stream = UnixStream::connect(path).await?;
        finish(stream, self.inner, false).await
    }

    /// Start a server session over an accepted Unix-socket stream.
    ///
    /// This awaits the QMux handshake (the peer's transport parameters), bounded
    /// by [`handshake_timeout`](Self::handshake_timeout). Drive each connection
    /// with `tokio::spawn` so a slow or non-cooperative peer can't stall your
    /// `listener.accept()` loop.
    pub async fn accept(self, stream: UnixStream) -> Result<Session, Error> {
        finish(stream, self.inner, true).await
    }
}

async fn finish(
    stream: UnixStream,
    config: crate::Config,
    is_server: bool,
) -> Result<Session, Error> {
    // `build_stream_session` awaits the peer's transport parameters before
    // returning, so `protocol()` and `path()` are resolved on the session we hand
    // back (bounded by the config's handshake timeout).
    build_stream_session(stream, config, is_server).await
}
