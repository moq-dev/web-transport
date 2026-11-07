use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use crate::proto::ConnectRequest;
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use noq::crypto::rustls::QuicClientConfig;
use rustls::{client::danger::ServerCertVerifier, pki_types::CertificateDer};
use tokio::net::lookup_host;
use url::Host;

use crate::crypto;
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use crate::ALPN;
use crate::{ClientError, Session};

/// Congestion control algorithm to use for the connection.
///
/// Different algorithms make different tradeoffs between throughput and latency.
pub enum CongestionControl {
    /// Use the default congestion control algorithm (typically CUBIC).
    Default,
    /// Optimize for throughput (typically CUBIC).
    Throughput,
    /// Optimize for low latency (typically BBR).
    LowLatency,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
/// Construct a WebTransport [Client] using sane defaults.
///
/// This is optional; advanced users may use [Client::new] directly.
#[derive(Clone)]
pub struct ClientBuilder {
    provider: crypto::Provider,
    congestion_controller:
        Option<Arc<dyn noq::congestion::ControllerFactory + Send + Sync + 'static>>,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl ClientBuilder {
    /// Create a Client builder, which can be used to establish multiple [Session]s.
    pub fn new() -> Self {
        Self {
            provider: crypto::default_provider(),
            congestion_controller: None,
        }
    }

    /// Enable the specified congestion controller.
    pub fn with_congestion_control(mut self, algorithm: CongestionControl) -> Self {
        self.congestion_controller = match algorithm {
            CongestionControl::LowLatency => Some(Arc::new(noq::congestion::Bbr3Config::default())),
            // TODO BBR is also higher throughput in theory.
            CongestionControl::Throughput => {
                Some(Arc::new(noq::congestion::CubicConfig::default()))
            }
            CongestionControl::Default => None,
        };

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
            .builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        self.build(crypto)
    }

    /// Supply certificates for accepted servers instead of using root CAs.
    pub fn with_server_certificates(
        self,
        certs: Vec<CertificateDer>,
    ) -> Result<Client, ClientError> {
        let hashes = certs.iter().map({
            let provider = self.provider.clone();
            move |cert| crypto::sha256(&provider, cert).as_ref().to_vec()
        });

        self.with_server_certificate_hashes(hashes.collect())
    }

    /// Supply sha256 hashes for accepted certificates instead of using root CAs.
    pub fn with_server_certificate_hashes(
        self,
        hashes: Vec<Vec<u8>>,
    ) -> Result<Client, ClientError> {
        // Use a custom fingerprint verifier.
        let fingerprints = Arc::new(ServerFingerprints {
            provider: self.provider.clone(),
            fingerprints: hashes,
        });

        // Configure the crypto client.
        let crypto = self
            .builder()
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

    fn builder(&self) -> rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier> {
        rustls::ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
    }

    fn build(self, mut crypto: rustls::ClientConfig) -> Result<Client, ClientError> {
        crypto.alpn_protocols = vec![ALPN.as_bytes().to_vec()];

        let client_config = QuicClientConfig::try_from(crypto).unwrap();
        let mut client_config = noq::ClientConfig::new(Arc::new(client_config));

        let mut transport = noq::TransportConfig::default();
        if let Some(cc) = &self.congestion_controller {
            transport.congestion_controller_factory(cc.clone());
        }

        client_config.transport_config(transport.into());

        Ok(Client::from_config(client_config))
    }
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
/// Builder for dangerous TLS configuration options.
///
/// This builder provides access to potentially insecure TLS configurations.
/// These options should only be used when you understand the security implications,
/// such as in local development or over a secure VPN connection.
pub struct DangerousClientBuilder {
    inner: ClientBuilder,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl DangerousClientBuilder {
    /// Disable certificate verification entirely.
    ///
    /// This makes the connection vulnerable to man-in-the-middle attacks.
    /// Only use this in secure environments, such as in local development or over a VPN connection.
    ///
    /// This method is safe in the Rust sense (no memory unsafety), but dangerous in the
    /// security sense, hence the explicit `dangerous()` builder requirement.
    pub fn with_no_certificate_verification(self) -> Result<Client, ClientError> {
        let noop = NoCertificateVerification(self.inner.provider.clone());

        let crypto = self
            .inner
            .builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(noop))
            .with_no_client_auth();

        self.inner.build(crypto)
    }
}

/// A client for connecting to a WebTransport server.
#[derive(Clone, Debug)]
pub struct Client {
    endpoints: Arc<Mutex<Binding>>,
    config: noq::ClientConfig,
}

impl Client {
    /// Manually create a client via a Noq endpoint and config.
    ///
    /// The ALPN MUST be set to [ALPN].
    pub fn new(endpoint: noq::Endpoint, config: noq::ClientConfig) -> Self {
        Self {
            endpoints: Arc::new(Mutex::new(Binding::Custom(endpoint))),
            config,
        }
    }

    /// Connect to the server.
    pub async fn connect(
        &self,
        request: impl Into<ConnectRequest>,
    ) -> Result<Session, ClientError> {
        let request = request.into();
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
        Session::connect(conn, request).await
    }
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl Client {
    /// Create a client that lazily binds one socket for each server address family.
    ///
    /// The config must negotiate [`ALPN`]; socket setup errors are returned by [`Self::connect`].
    fn from_config(config: noq::ClientConfig) -> Self {
        Self {
            endpoints: Arc::new(Mutex::new(Binding::Default {
                ipv4: None,
                ipv6: None,
                runtime: ClientRuntime::current(),
            })),
            config,
        }
    }
}

#[derive(Debug)]
enum Binding {
    Custom(noq::Endpoint),
    #[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
    Default {
        ipv4: Option<noq::Endpoint>,
        ipv6: Option<noq::Endpoint>,
        runtime: Option<ClientRuntime>,
    },
}

#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
impl Binding {
    fn endpoints(&self) -> impl Iterator<Item = &noq::Endpoint> {
        match self {
            Self::Custom(endpoint) => [Some(endpoint), None],
            #[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
            Self::Default { ipv4, ipv6, .. } => [ipv4.as_ref(), ipv6.as_ref()],
        }
        .into_iter()
        .flatten()
    }
}

impl Binding {
    fn connect(
        &mut self,
        config: noq::ClientConfig,
        remote: SocketAddr,
        host: &str,
    ) -> Result<noq::Connecting, ClientError> {
        let (endpoint, remote) = match self {
            Binding::Custom(endpoint) => (endpoint, remote),
            #[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
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
                    *slot = Some(
                        runtime
                            .bind(remote)
                            .map_err(|err| ClientError::IoError(Arc::new(err)))?,
                    );
                }
                (slot.as_mut().unwrap(), remote)
            }
        };
        Ok(endpoint.connect_with(config, remote, host)?)
    }
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
#[derive(Debug)]
struct ClientRuntime {
    inner: Arc<dyn noq::Runtime>,
    tokio: Option<tokio::runtime::Handle>,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl ClientRuntime {
    fn current() -> Option<Self> {
        Some(Self {
            inner: noq::default_runtime()?,
            tokio: tokio::runtime::Handle::try_current().ok(),
        })
    }

    fn bind(&self, remote: SocketAddr) -> std::io::Result<noq::Endpoint> {
        // TokioRuntime registers sockets and spawns drivers in the current context.
        let _guard = self.tokio.as_ref().map(tokio::runtime::Handle::enter);
        bind_endpoint(remote, self.inner.clone())
    }
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
fn bind_endpoint(
    remote: SocketAddr,
    runtime: Arc<dyn noq::Runtime>,
) -> std::io::Result<noq::Endpoint> {
    let bind: SocketAddr = if remote.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = bind_socket(bind)?;
    noq::Endpoint::new(noq::EndpointConfig::default(), None, socket, runtime)
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
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

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl Default for Client {
    fn default() -> Self {
        ClientBuilder::new().with_system_roots().unwrap()
    }
}

#[cfg_attr(not(any(feature = "aws-lc-rs", feature = "ring")), allow(dead_code))]
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
        let cert_hash = crypto::sha256(&self.provider, end_entity);
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

#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
mod tests;
