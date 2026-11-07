use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::crypto::QuicClientConfig;
use crate::proto::ConnectRequest;
use rustls::{client::danger::ServerCertVerifier, pki_types::CertificateDer};
use tokio::net::lookup_host;
use url::Host;

use crate::crypto;
use crate::ALPN;
use crate::{ClientError, Session};

// Match Quinn's client socket setup, including dual-stack operation on Windows.
fn client_socket(addr: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if addr.is_ipv6() {
        if let Err(error) = socket.set_only_v6(false) {
            tracing::debug!(%error, "unable to make socket dual-stack");
        }
    }
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

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
    congestion_controller: Option<&ControllerFactory>,
) -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    if let Some(cc) = congestion_controller {
        transport.congestion_controller_factory(cc.clone());
    }

    Arc::new(transport)
}

/// Construct a WebTransport [Client] using sane defaults.
///
/// This is optional; advanced users may use [Client::new] directly.
#[derive(Clone)]
pub struct ClientBuilder {
    provider: Result<crypto::Provider, crypto::UnsupportedCpu>,
    congestion_controller: Option<ControllerFactory>,
}

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
        client_config.transport_config(transport_config(self.congestion_controller.as_ref()));

        let client = quinn::Endpoint::new(
            crypto::endpoint_config()?,
            None,
            client_socket("[::]:0".parse().unwrap())
                .map_err(|e| ClientError::IoError(Arc::new(e)))?,
            quinn::default_runtime().ok_or_else(|| {
                ClientError::IoError(Arc::new(std::io::Error::other("no async runtime found")))
            })?,
        )
        .map_err(|e| ClientError::IoError(Arc::new(e)))?;
        Ok(Client {
            endpoint: client,
            config: client_config,
        })
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
    endpoint: quinn::Endpoint,
    config: quinn::ClientConfig,
}

impl Client {
    /// Manually create a client via a Quinn endpoint and config.
    ///
    /// The ALPN MUST be set to [ALPN].
    pub fn new(endpoint: quinn::Endpoint, config: quinn::ClientConfig) -> Self {
        Self { endpoint, config }
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
            .endpoint
            .connect_with(self.config.clone(), remote, &host)?;
        let conn = conn.await?;

        // Connect with the connection we established.
        Session::connect(conn, request).await
    }
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
mod tests {
    use super::*;

    #[test]
    fn client_socket_preserves_quinn_dual_stack_default() {
        let socket = client_socket("[::]:0".parse().unwrap()).unwrap();
        assert!(!socket2::SockRef::from(&socket).only_v6().unwrap());
    }
}
