use std::io;
use std::sync::Arc;

use futures::StreamExt;
use futures::{future::BoxFuture, stream::FuturesUnordered};

use crate::{ez, h3};

/// An error returned when receiving a new WebTransport session.
#[derive(thiserror::Error, Debug)]
pub enum ServerError {
    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("settings error: {0}")]
    Settings(#[from] h3::SettingsError),

    #[error("connect error: {0}")]
    Connect(#[from] h3::ConnectError),
}

impl From<std::io::Error> for ServerError {
    fn from(err: std::io::Error) -> Self {
        ServerError::Io(Arc::new(err))
    }
}

/// Construct a WebTransport server using sane defaults.
pub struct ServerBuilder<M: ez::Metrics = ez::DefaultMetrics, S = ez::ServerInit>(
    ez::ServerBuilder<M, S>,
);

impl Default for ServerBuilder<ez::DefaultMetrics> {
    fn default() -> Self {
        Self(ez::ServerBuilder::default())
    }
}

impl ServerBuilder<ez::DefaultMetrics, ez::ServerInit> {
    /// Create a new server builder with custom metrics.
    ///
    /// Use [ServerBuilder::default] if you don't care about metrics.
    pub fn with_metrics<M: ez::Metrics>(m: M) -> ServerBuilder<M, ez::ServerInit> {
        ServerBuilder(ez::ServerBuilder::with_metrics(m))
    }
}

impl<M: ez::Metrics> ServerBuilder<M, ez::ServerInit> {
    /// Configure the server to use the provided QUIC listener.
    pub fn with_listener(
        self,
        listener: tokio_quiche::socket::QuicListener,
    ) -> ServerBuilder<M, ez::ServerWithListener> {
        ServerBuilder::<M, ez::ServerWithListener>(self.0.with_listener(listener))
    }

    /// Listen for incoming packets on the given socket.
    pub fn with_socket(
        self,
        socket: std::net::UdpSocket,
    ) -> io::Result<ServerBuilder<M, ez::ServerWithListener>> {
        Ok(ServerBuilder::<M, ez::ServerWithListener>(
            self.0.with_socket(socket)?,
        ))
    }

    /// Listen for incoming packets on the given address.
    pub fn with_bind<A: std::net::ToSocketAddrs>(
        self,
        addrs: A,
    ) -> io::Result<ServerBuilder<M, ez::ServerWithListener>> {
        Ok(ServerBuilder::<M, ez::ServerWithListener>(
            self.0.with_bind(addrs)?,
        ))
    }

    /// Use the provided [Settings] instead of the defaults.
    pub fn with_settings(self, settings: ez::Settings) -> Self {
        Self(self.0.with_settings(settings))
    }
}

impl<M: ez::Metrics> ServerBuilder<M, ez::ServerWithListener> {
    /// Configure the server to use the provided QUIC listener.
    pub fn with_listener(self, listener: tokio_quiche::socket::QuicListener) -> Self {
        Self(self.0.with_listener(listener))
    }

    /// Listen for incoming packets on the given socket.
    pub fn with_socket(self, socket: std::net::UdpSocket) -> io::Result<Self> {
        Ok(Self(self.0.with_socket(socket)?))
    }

    /// Listen for incoming packets on the given address.
    pub fn with_bind<A: std::net::ToSocketAddrs>(self, addrs: A) -> io::Result<Self> {
        Ok(Self(self.0.with_bind(addrs)?))
    }

    /// Use the provided [Settings] instead of the defaults.
    pub fn with_settings(self, settings: ez::Settings) -> Self {
        Self(self.0.with_settings(settings))
    }

    /// Configure the server to use the specified certificate for TLS.
    pub fn with_cert<'a>(self, tls: ez::CertificatePath<'a>) -> io::Result<Server<M>> {
        Ok(Server::new(self.0.with_cert(tls)?))
    }
}

/// A WebTransport server that accepts new sessions.
pub struct Server<M: ez::Metrics = ez::DefaultMetrics> {
    inner: ez::Server<M>,
    accept: FuturesUnordered<BoxFuture<'static, Result<h3::Request, ServerError>>>,
}

impl<M: ez::Metrics> Server<M> {
    /// Wrap an underlying QUIC server, abstracting away the HTTP/3 handshake required for WebTransport.
    ///
    /// **Note**: The ALPN must be set to `h3`.
    pub fn new(inner: ez::Server<M>) -> Self {
        Self {
            inner,
            accept: Default::default(),
        }
    }

    /// Accept a new WebTransport session [h3::Request] from a client.
    ///
    /// Returns [h3::Request] which allows the server to inspect the URL and decide whether to accept or reject the session.
    pub async fn accept(&mut self) -> Option<h3::Request> {
        loop {
            tokio::select! {
                Some(conn) = self.inner.accept() => self.accept.push(Box::pin(h3::Request::accept(conn))),
                Some(res) = self.accept.next() => {
                    match res {
                        Ok(session) => return Some(session),
                        Err(err) => tracing::warn!("ignoring failed HTTP/3 handshake: {}", err),
                    }
                }
                else => return None,
            }
        }
    }
}
