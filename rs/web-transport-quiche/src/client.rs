use std::sync::Arc;
use web_transport_proto::ConnectRequest;

use crate::{
    ez::{self, DefaultMetrics, Metrics},
    h3, Connection, Settings,
};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(thiserror::Error, Debug, Clone)]
pub enum ClientError {
    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("connection error: {0}")]
    Connection(#[from] ez::ConnectionError),

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

    /// Optional: Use a client certificate for mTLS.
    pub fn with_single_cert(
        self,
        chain: Vec<ez::CertificateDer<'static>>,
        key: ez::PrivateKeyDer<'static>,
    ) -> Self {
        Self(self.0.with_single_cert(chain, key))
    }

    /// Verify the server certificate against an explicit set of root
    /// certificates instead of the system trust store.
    pub fn with_root_certificates(self, roots: Vec<ez::CertificateDer<'static>>) -> Self {
        Self(self.0.with_root_certificates(roots))
    }

    /// Accept the server certificate only if the SHA-256 of its DER encoding
    /// matches one of the provided hashes, bypassing CA verification.
    ///
    /// This mirrors the browser's `serverCertificateHashes` option and is the
    /// usual way to reach a relay using a short-lived self-signed certificate.
    pub fn with_server_certificate_hashes(self, hashes: Vec<[u8; 32]>) -> Self {
        Self(self.0.with_server_certificate_hashes(hashes))
    }

    /// Override the TLS server name (SNI) used for the handshake and hostname
    /// verification.
    ///
    /// Defaults to the host in the URL passed to [ClientBuilder::connect], which
    /// is almost always what you want. Set it explicitly to reach a server whose
    /// certificate names a different host than the one you're dialing.
    ///
    /// The URL is otherwise unaffected: the `:authority` sent in the CONNECT
    /// request still comes from the URL.
    pub fn with_server_name(self, name: impl Into<String>) -> Self {
        Self(self.0.with_server_name(name))
    }

    /// Send a PING on this interval, keeping an idle connection alive.
    ///
    /// Disabled by default. This must be shorter than the peer's
    /// [Settings::max_idle_timeout] to have any effect; a third of it is a
    /// reasonable choice.
    pub fn with_keep_alive(self, interval: std::time::Duration) -> Self {
        Self(self.0.with_keep_alive(interval))
    }

    /// Enable UDP generic segmentation offload (GSO), on by default.
    ///
    /// GSO cuts syscall overhead at high throughput by handing the kernel
    /// several packets at once, but some NICs and virtual network stacks
    /// mishandle it. Turn it off if large sends are being dropped.
    ///
    /// Only Linux supports GSO; elsewhere this does nothing.
    pub fn with_gso(self, enabled: bool) -> Self {
        Self(self.0.with_gso(enabled))
    }

    /// Connect to the WebTransport server at the given URL.
    ///
    /// The URL host is resolved via DNS and the first address is used. Resolve
    /// it yourself and call [ClientBuilder::connect_to] to choose the address,
    /// for example to prefer a particular address family.
    ///
    /// DNS resolution and socket setup happen eagerly. The returned [Connecting]
    /// has an [established](Connecting::established) method to complete the full handshake
    /// (TLS + SETTINGS + CONNECT).
    ///
    /// This takes ownership because the underlying quiche implementation doesn't support reusing the same socket.
    pub async fn connect(
        self,
        request: impl Into<ConnectRequest>,
    ) -> Result<Connecting, ClientError> {
        let request = request.into();
        let (host, port) = Self::target(&request)?;

        let connecting = self.0.connect(&host, port).await?;

        Ok(Connecting {
            connecting,
            request,
        })
    }

    /// Connect to the WebTransport server at an already-resolved address.
    ///
    /// The URL still supplies the request itself, including the TLS server name
    /// unless [ClientBuilder::with_server_name] overrides it. Only the address
    /// to dial is taken from `remote`.
    ///
    /// This takes ownership because the underlying quiche implementation doesn't support reusing the same socket.
    pub async fn connect_to(
        mut self,
        request: impl Into<ConnectRequest>,
        remote: std::net::SocketAddr,
    ) -> Result<Connecting, ClientError> {
        let request = request.into();
        let (host, _) = Self::target(&request)?;

        // `ez::connect_to` has no host to fall back on, so pin down the name the
        // URL implies unless the caller already chose one.
        self.0 = self.0.with_default_server_name(host);

        let connecting = self.0.connect_to(remote).await?;

        Ok(Connecting {
            connecting,
            request,
        })
    }

    /// The host and port to dial for a request.
    fn target(request: &ConnectRequest) -> Result<(String, u16), ClientError> {
        // `Host` renders IPv6 in URL form, bracketed, which is not what a
        // resolver or a TLS server name wants.
        let host = match request.url.host() {
            Some(url::Host::Domain(host)) => host.to_string(),
            Some(url::Host::Ipv4(ip)) => ip.to_string(),
            Some(url::Host::Ipv6(ip)) => ip.to_string(),
            None => return Err(ClientError::InvalidUrl(request.url.to_string())),
        };

        Ok((host, request.url.port().unwrap_or(443)))
    }
}

/// A WebTransport connection that is still completing the handshake.
///
/// Call [Connecting::established] to wait for the full handshake to complete
/// (TLS + SETTINGS + CONNECT).
pub struct Connecting {
    connecting: ez::Connecting,
    request: ConnectRequest,
}

impl Connecting {
    /// Wait for the full handshake to complete (TLS + SETTINGS + CONNECT).
    pub async fn established(self) -> Result<Connection, ClientError> {
        let conn = self.connecting.established().await?;
        Connection::connect(conn, self.request).await
    }
}
