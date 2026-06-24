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

    /// Connect to the socket at `path` and start a client session.
    pub async fn connect(self, path: impl AsRef<Path>) -> Result<Session, Error> {
        let stream = UnixStream::connect(path).await?;
        finish(stream, self.inner, false).await
    }

    /// Start a server session over an accepted Unix-socket stream.
    pub async fn accept(self, stream: UnixStream) -> Result<Session, Error> {
        finish(stream, self.inner, true).await
    }
}

async fn finish(
    stream: UnixStream,
    config: crate::Config,
    is_server: bool,
) -> Result<Session, Error> {
    let session = build_stream_session(stream, config, is_server)?;
    // Resolve the protocol and peer path before returning. Both arrive in the
    // peer's transport parameters, so awaiting one (or the params/close) is
    // enough for a server to read `path()` right after `accept`.
    session.negotiated().await;
    session.peer_path().await;
    Ok(session)
}
