use std::sync::Arc;
use url::Url;

use crate::{
    ez::{self, CertificatePath, DefaultMetrics, Metrics},
    h3, Connection, Settings,
};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("settings error: {0}")]
    Settings(#[from] h3::SettingsError),

    #[error("connect error: {0}")]
    Connect(#[from] h3::ConnectError),

    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

impl From<std::io::Error> for ClientError {
    fn from(err: std::io::Error) -> Self {
        ClientError::Io(Arc::new(err))
    }
}

/// Construct a WebTransport client using sane defaults.
pub struct ClientBuilder<M: Metrics = DefaultMetrics>(ez::ClientBuilder<M>);

impl Default for ClientBuilder<DefaultMetrics> {
    fn default() -> Self {
        Self(ez::ClientBuilder::default())
    }
}

impl ClientBuilder<DefaultMetrics> {
    /// Create a new client builder with custom metrics.
    ///
    /// Use [ClientBuilder::default] if you don't care about metrics.
    pub fn with_metrics<M: Metrics>(m: M) -> ClientBuilder<M> {
        ClientBuilder(ez::ClientBuilder::with_metrics(m))
    }
}

impl<M: Metrics> ClientBuilder<M> {
    /// Listen for incoming packets on the given socket.
    ///
    /// Defaults to an ephemeral port if not specified.
    pub fn with_socket(self, socket: std::net::UdpSocket) -> Result<Self, ClientError> {
        Ok(Self(self.0.with_socket(socket)?))
    }

    /// Listen for incoming packets on the given address.
    ///
    /// Defaults to an ephemeral port if not specified.
    pub fn with_bind<A: std::net::ToSocketAddrs>(self, addrs: A) -> Result<Self, ClientError> {
        // We use std to avoid async
        let socket = std::net::UdpSocket::bind(addrs)?;
        self.with_socket(socket)
    }

    /// Use the provided [Settings] instead of the defaults.
    ///
    /// **WARNING**: [Settings::verify_peer] is set to false by default.
    /// This will completely bypass certificate verification and is generally not recommended.
    pub fn with_settings(self, settings: Settings) -> Self {
        Self(self.0.with_settings(settings))
    }

    /// Optional: Use a client certificate for TLS.
    pub fn with_cert(self, tls: CertificatePath<'_>) -> Result<Self, ClientError> {
        Ok(Self(self.0.with_cert(tls)?))
    }

    /// Connect to the WebTransport server at the given URL.
    ///
    /// This takes ownership because the underlying quiche implementation doesn't support reusing the same socket.
    pub async fn connect(self, url: Url) -> Result<Connection, ClientError> {
        let port = url.port().unwrap_or(443);

        let host = match url.host() {
            Some(host) => host.to_string(),
            None => return Err(ClientError::InvalidUrl(url.to_string())),
        };

        let conn = self.0.connect(&host, port).await?;

        Connection::connect(conn, url).await
    }
}
