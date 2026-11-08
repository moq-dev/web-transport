use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use crate::crypto::QuicClientConfig;
use crate::proto::ConnectRequest;
use rustls::{client::danger::ServerCertVerifier, pki_types::CertificateDer};
use tokio::net::lookup_host;
use url::Host;

use crate::crypto;
use crate::ALPN;
use crate::{ClientError, Session};

/// Congestion control algorithm to use for the connection.
///
/// Different algorithms make different tradeoffs between throughput and latency.
pub enum CongestionControl {
    /// Keep the transport configuration's controller (Quinn defaults to CUBIC).
    Default,
    /// Optimize for throughput (typically CUBIC).
    Throughput,
    /// Optimize for low latency (typically BBR).
    LowLatency,
}

pub(crate) type ControllerFactory =
    Arc<dyn quinn::congestion::ControllerFactory + Send + Sync + 'static>;

/// Turn a [CongestionControl] choice into the factory quinn wants.
pub(crate) fn controller_factory(algorithm: CongestionControl) -> Option<ControllerFactory> {
    match algorithm {
        CongestionControl::LowLatency => Some(Arc::new(quinn::congestion::BbrConfig::default())),
        // TODO BBR is also higher throughput in theory.
        CongestionControl::Throughput => Some(Arc::new(quinn::congestion::CubicConfig::default())),
        CongestionControl::Default => None,
    }
}

/// The transport config shared by both builders, so the client and server can't
/// drift on which knobs actually get applied.
pub(crate) fn transport_config(
    mut transport: quinn::TransportConfig,
    congestion_controller: Option<&ControllerFactory>,
) -> Arc<quinn::TransportConfig> {
    if let Some(cc) = congestion_controller {
        transport.congestion_controller_factory(cc.clone());
    }

    Arc::new(transport)
}

/// Construct a WebTransport [Client] using sane defaults.
///
/// This is optional; advanced users may use [Client::new] directly.
/// Build a client before cloning it to share its endpoints and configuration.
pub struct ClientBuilder {
    provider: Result<crypto::Provider, crypto::UnsupportedCpu>,
    transport: quinn::TransportConfig,
    congestion_controller: Option<ControllerFactory>,
}

impl ClientBuilder {
    /// Create a Client builder, which can be used to establish multiple [Session]s.
    pub fn new() -> Self {
        Self {
            provider: crypto::default_provider(),
            transport: quinn::TransportConfig::default(),
            congestion_controller: None,
        }
    }

    /// Use the supplied QUIC transport configuration instead of Quinn's defaults.
    ///
    /// An explicit [`Self::with_congestion_control`] choice overrides only its
    /// congestion controller, regardless of the order these methods are called.
    pub fn with_transport_config(mut self, transport: quinn::TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    /// Override the transport configuration's congestion controller.
    ///
    /// [`CongestionControl::Default`] preserves the supplied configuration's
    /// controller, or Quinn's default when no configuration is supplied.
    pub fn with_congestion_control(mut self, algorithm: CongestionControl) -> Self {
        self.congestion_controller = controller_factory(algorithm);
        self
    }

    /// Accept any certificate from the server if it uses a known root CA.
    pub fn with_system_roots(self) -> Result<Client, ClientError> {
        let mut roots = rustls::RootCertStore::empty();

        let native = rustls_native_certs::load_native_certs();

        // Log any errors that occurred while loading the native root certificates.
        for err in native.errors {
            tracing::warn!(?err, "failed to load root cert");
        }

        // Add the platform's native root certificates.
        for cert in native.certs {
            if let Err(err) = roots.add(cert) {
                tracing::warn!(?err, "failed to add root cert");
            }
        }

        let crypto = self
            .builder()?
            .with_root_certificates(roots)
            .with_no_client_auth();

        self.build(crypto)
    }

    /// Supply certificates for accepted servers instead of using root CAs.
    pub fn with_server_certificates(
        self,
        certs: Vec<CertificateDer>,
    ) -> Result<Client, ClientError> {
        let provider = self.provider.clone()?;
        let hashes = certs
            .iter()
            .map(move |cert| crypto::sha256(&provider, cert).map(|hash| hash.as_ref().to_vec()));

        self.with_server_certificate_hashes(hashes.collect::<Result<_, _>>()?)
    }

    /// Supply sha256 hashes for accepted certificates instead of using root CAs.
    pub fn with_server_certificate_hashes(
        self,
        hashes: Vec<Vec<u8>>,
    ) -> Result<Client, ClientError> {
        // Use a custom fingerprint verifier.
        let fingerprints = Arc::new(ServerFingerprints {
            provider: self.provider.clone()?,
            fingerprints: hashes,
        });

        // Configure the crypto client.
        let crypto = self
            .builder()?
            .dangerous()
            .with_custom_certificate_verifier(fingerprints.clone())
            .with_no_client_auth();

        self.build(crypto)
    }

    /// Access dangerous configuration options.
    ///
    /// This method returns a builder that provides access to potentially insecure
    /// TLS configurations. These options are opt-in and require explicit acknowledgment
    /// through the builder pattern, making the security implications clear at the call site.
    pub fn dangerous(self) -> DangerousClientBuilder {
        DangerousClientBuilder { inner: self }
    }

    fn builder(
        &self,
    ) -> Result<rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier>, ClientError>
    {
        Ok(
            rustls::ClientConfig::builder_with_provider(self.provider.clone()?)
                .with_protocol_versions(&[&rustls::version::TLS13])?,
        )
    }

    fn build(self, mut crypto: rustls::ClientConfig) -> Result<Client, ClientError> {
        crypto.alpn_protocols = vec![ALPN.as_bytes().to_vec()];

        let client_config = QuicClientConfig::try_from(crypto)?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(client_config));
        client_config.transport_config(transport_config(
            self.transport,
            self.congestion_controller.as_ref(),
        ));

        Ok(Client::from(client_config))
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for dangerous TLS configuration options.
///
/// This builder provides access to potentially insecure TLS configurations.
/// These options should only be used when you understand the security implications,
/// such as in local development or over a secure VPN connection.
pub struct DangerousClientBuilder {
    inner: ClientBuilder,
}

impl DangerousClientBuilder {
    /// Disable certificate verification entirely.
    ///
    /// This makes the connection vulnerable to man-in-the-middle attacks.
    /// Only use this in secure environments, such as in local development or over a VPN connection.
    ///
    /// This method is safe in the Rust sense (no memory unsafety), but dangerous in the
    /// security sense, hence the explicit `dangerous()` builder requirement.
    pub fn with_no_certificate_verification(self) -> Result<Client, ClientError> {
        let noop = NoCertificateVerification(self.inner.provider.clone()?);

        let crypto = self
            .inner
            .builder()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(noop))
            .with_no_client_auth();

        self.inner.build(crypto)
    }
}

/// A client for connecting to a WebTransport server.
#[derive(Clone, Debug)]
pub struct Client {
    endpoints: Arc<Mutex<ClientEndpoints>>,
    config: quinn::ClientConfig,
}

impl Client {
    /// Manually create a client via a Quinn endpoint and config.
    ///
    /// The ALPN MUST be set to [ALPN].
    pub fn new(endpoint: quinn::Endpoint, config: quinn::ClientConfig) -> Self {
        Self {
            endpoints: Arc::new(Mutex::new(ClientEndpoints {
                binding: Binding::Custom(endpoint),
                closed: false,
            })),
            config,
        }
    }

    /// Connect to the server.
    pub async fn connect(
        &self,
        request: impl Into<ConnectRequest>,
    ) -> Result<Session, ClientError> {
        let request = request.into();
        if self.endpoints.lock().unwrap().closed {
            return Err(quinn::ConnectError::EndpointStopping.into());
        }

        let port = request.url.port().unwrap_or(443);

        // TODO error on username:password in host
        let (host, remote) = match request
            .url
            .host()
            .ok_or_else(|| ClientError::InvalidDnsName("".to_string()))?
        {
            Host::Domain(domain) => {
                let domain = domain.to_string();
                // Look up the DNS entry.
                let mut remotes = match lookup_host((domain.clone(), port)).await {
                    Ok(remotes) => remotes,
                    Err(_) => return Err(ClientError::InvalidDnsName(domain)),
                };

                // Return the first entry.
                let remote = match remotes.next() {
                    Some(remote) => remote,
                    None => return Err(ClientError::InvalidDnsName(domain)),
                };

                (domain, remote)
            }
            Host::Ipv4(ipv4) => (ipv4.to_string(), SocketAddr::new(IpAddr::V4(ipv4), port)),
            Host::Ipv6(ipv6) => (ipv6.to_string(), SocketAddr::new(IpAddr::V6(ipv6), port)),
        };

        // Connect to the server using the addr we just resolved.
        let conn = self
            .endpoints
            .lock()
            .unwrap()
            .connect(self.config.clone(), remote, &host)?;
        let conn = conn.await?;

        // Connect with the connection we established.
        let session = Session::connect(conn, request).await?;
        if self.endpoints.lock().unwrap().closed {
            return Err(quinn::ConnectionError::LocallyClosed.into());
        }
        Ok(session)
    }

    /// Close all bound endpoints and reject future connections from every clone.
    pub fn close(&self, code: quinn::VarInt, reason: &[u8]) {
        let mut endpoints = self.endpoints.lock().unwrap();
        if endpoints.closed {
            return;
        }
        endpoints.closed = true;
        for endpoint in endpoints.binding.endpoints() {
            endpoint.close(code, reason);
        }
    }

    /// Wait for currently bound endpoints to become idle; close first to prevent new connections.
    pub async fn wait_idle(&self) {
        let endpoints: Vec<_> = self
            .endpoints
            .lock()
            .unwrap()
            .binding
            .endpoints()
            .cloned()
            .collect();
        futures::future::join_all(endpoints.iter().map(quinn::Endpoint::wait_idle)).await;
    }
}

impl From<quinn::ClientConfig> for Client {
    /// Create a client that lazily binds one socket for each server address family.
    ///
    /// The config must negotiate [`ALPN`]; socket setup errors are returned by [`Self::connect`].
    fn from(config: quinn::ClientConfig) -> Self {
        Self {
            endpoints: Arc::new(Mutex::new(ClientEndpoints {
                binding: Binding::Default {
                    ipv4: None,
                    ipv6: None,
                    runtime: ClientRuntime::current(),
                },
                closed: false,
            })),
            config,
        }
    }
}

#[derive(Debug)]
struct ClientEndpoints {
    binding: Binding,
    closed: bool,
}

#[derive(Debug)]
enum Binding {
    Custom(quinn::Endpoint),
    Default {
        ipv4: Option<quinn::Endpoint>,
        ipv6: Option<quinn::Endpoint>,
        runtime: Option<ClientRuntime>,
    },
}

impl Binding {
    fn endpoints(&self) -> impl Iterator<Item = &quinn::Endpoint> {
        match self {
            Self::Custom(endpoint) => [Some(endpoint), None],
            Self::Default { ipv4, ipv6, .. } => [ipv4.as_ref(), ipv6.as_ref()],
        }
        .into_iter()
        .flatten()
    }
}

impl ClientEndpoints {
    fn connect(
        &mut self,
        config: quinn::ClientConfig,
        remote: SocketAddr,
        host: &str,
    ) -> Result<quinn::Connecting, ClientError> {
        if self.closed {
            return Err(quinn::ConnectError::EndpointStopping.into());
        }
        // Binding and starting the connection share close's lock, so close
        // cannot miss a newly created endpoint or an in-progress connection.
        let (endpoint, remote) = match &mut self.binding {
            Binding::Custom(endpoint) => (endpoint, remote),
            Binding::Default {
                ipv4,
                ipv6,
                runtime,
            } => {
                let remote = match remote {
                    SocketAddr::V6(addr) => addr
                        .ip()
                        .to_ipv4_mapped()
                        .map(|ip| SocketAddr::from((ip, addr.port())))
                        .unwrap_or(remote),
                    _ => remote,
                };
                let slot = if remote.is_ipv4() { ipv4 } else { ipv6 };
                if slot.is_none() {
                    if runtime.is_none() {
                        *runtime = ClientRuntime::current();
                    }
                    let runtime = runtime.as_ref().ok_or_else(|| {
                        ClientError::IoError(Arc::new(std::io::Error::other(
                            "no async runtime found",
                        )))
                    })?;
                    *slot = Some(runtime.bind(remote)?);
                }
                (slot.as_mut().unwrap(), remote)
            }
        };
        Ok(endpoint.connect_with(config, remote, host)?)
    }
}

#[derive(Debug)]
struct ClientRuntime {
    inner: Arc<dyn quinn::Runtime>,
    tokio: Option<tokio::runtime::Handle>,
}

impl ClientRuntime {
    fn current() -> Option<Self> {
        Some(Self {
            inner: quinn::default_runtime()?,
            tokio: tokio::runtime::Handle::try_current().ok(),
        })
    }

    fn bind(&self, remote: SocketAddr) -> Result<quinn::Endpoint, ClientError> {
        // TokioRuntime registers sockets and spawns drivers in the current context.
        let _guard = self.tokio.as_ref().map(tokio::runtime::Handle::enter);
        bind_endpoint(remote, self.inner.clone())
    }
}

fn bind_endpoint(
    remote: SocketAddr,
    runtime: Arc<dyn quinn::Runtime>,
) -> Result<quinn::Endpoint, ClientError> {
    let bind: SocketAddr = if remote.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let io = |err| ClientError::IoError(Arc::new(err));
    let socket = bind_socket(bind).map_err(io)?;
    quinn::Endpoint::new(crypto::endpoint_config()?, None, socket, runtime).map_err(io)
}

fn bind_socket(bind: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(bind),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if bind.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&bind.into())?;
    Ok(socket.into())
}

/// Panics if system-root setup fails, including on a CPU rind does not support.
impl Default for Client {
    fn default() -> Self {
        ClientBuilder::new().with_system_roots().unwrap()
    }
}

#[derive(Debug)]
struct ServerFingerprints {
    provider: crypto::Provider,
    fingerprints: Vec<Vec<u8>>,
}

impl ServerCertVerifier for ServerFingerprints {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let cert_hash = crypto::sha256(&self.provider, end_entity)?;
        if self
            .fingerprints
            .iter()
            .any(|fingerprint| fingerprint == cert_hash.as_ref())
        {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }

        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
pub struct NoCertificateVerification(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
pub(crate) mod tests;
