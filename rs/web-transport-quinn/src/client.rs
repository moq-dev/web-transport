use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::proto::ConnectRequest;
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use quinn::crypto::rustls::QuicClientConfig;
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
    /// Keep the transport configuration's controller (Quinn defaults to CUBIC).
    Default,
    /// Optimize for throughput (typically CUBIC).
    Throughput,
    /// Optimize for low latency (typically BBR).
    LowLatency,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
pub(crate) type ControllerFactory =
    Arc<dyn quinn::congestion::ControllerFactory + Send + Sync + 'static>;

/// Turn a [CongestionControl] choice into the factory quinn wants.
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
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
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
pub(crate) fn transport_config(
    mut transport: quinn::TransportConfig,
    congestion_controller: Option<&ControllerFactory>,
) -> Arc<quinn::TransportConfig> {
    if let Some(cc) = congestion_controller {
        transport.congestion_controller_factory(cc.clone());
    }

    Arc::new(transport)
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
/// Construct a WebTransport [Client] using sane defaults.
///
/// This is optional; advanced users may use [Client::new] directly.
/// Build a client before cloning it to share its endpoint and configuration.
pub struct ClientBuilder {
    provider: crypto::Provider,
    transport: quinn::TransportConfig,
    congestion_controller: Option<ControllerFactory>,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
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
        let mut client_config = quinn::ClientConfig::new(Arc::new(client_config));
        client_config.transport_config(transport_config(
            self.transport,
            self.congestion_controller.as_ref(),
        ));

        let client = quinn::Endpoint::client("[::]:0".parse().unwrap()).unwrap();
        Ok(Client {
            endpoint: client,
            config: client_config,
        })
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
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    pub(crate) fn builder() -> ClientBuilder {
        // Avoid a process-wide provider when both features are enabled.
        #[cfg(feature = "aws-lc-rs")]
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        #[cfg(all(feature = "ring", not(feature = "aws-lc-rs")))]
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        ClientBuilder {
            provider,
            transport: quinn::TransportConfig::default(),
            congestion_controller: None,
        }
    }

    struct CountingController(Arc<AtomicUsize>);

    impl quinn::congestion::ControllerFactory for CountingController {
        fn build(
            self: Arc<Self>,
            now: std::time::Instant,
            mtu: u16,
        ) -> Box<dyn quinn::congestion::Controller> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Arc::new(quinn::congestion::NewRenoConfig::default()).build(now, mtu)
        }
    }

    pub(crate) fn custom_transport() -> (quinn::TransportConfig, Arc<AtomicUsize>) {
        let built = Arc::new(AtomicUsize::new(0));
        let mut transport = quinn::TransportConfig::default();
        transport
            .max_idle_timeout(Some(Duration::from_secs(12).try_into().unwrap()))
            .keep_alive_interval(Some(Duration::from_secs(3)))
            .congestion_controller_factory(Arc::new(CountingController(built.clone())));
        (transport, built)
    }

    pub(crate) fn assert_transport(config: &impl std::fmt::Debug, custom: bool) {
        // Quinn exposes these settings only through Debug.
        let config = format!("{config:?}");
        let (idle, keepalive) = if custom {
            (
                "max_idle_timeout: Some(12000)",
                "keep_alive_interval: Some(3s)",
            )
        } else {
            ("max_idle_timeout: Some(30000)", "keep_alive_interval: None")
        };
        assert!(config.contains(idle), "{config}");
        assert!(config.contains(keepalive), "{config}");
    }

    fn finish(builder: ClientBuilder, path: u8) -> Client {
        match path {
            0 => builder.with_system_roots(),
            1 => builder.with_server_certificate_hashes(vec![]),
            2 => builder.with_server_certificates(vec![]),
            3 => builder.dangerous().with_no_certificate_verification(),
            _ => unreachable!(),
        }
        .unwrap()
    }

    #[tokio::test]
    async fn builder_transport_reaches_every_client_config() {
        for path in 0..4 {
            let client = finish(builder(), path);
            assert_transport(&client.config, false);

            for transport_first in [false, true] {
                for algorithm in [
                    CongestionControl::Default,
                    CongestionControl::LowLatency,
                    CongestionControl::Throughput,
                ] {
                    let overridden = !matches!(algorithm, CongestionControl::Default);
                    let (transport, built) = custom_transport();
                    let builder = builder();
                    let builder = if transport_first {
                        builder
                            .with_transport_config(transport)
                            .with_congestion_control(algorithm)
                    } else {
                        builder
                            .with_congestion_control(algorithm)
                            .with_transport_config(transport)
                    };
                    let client = finish(builder, path);
                    assert_transport(&client.config, true);

                    // Controller construction happens before any network/TLS result,
                    // so the system-root path needs no publicly trusted test server.
                    let connecting = client
                        .endpoint
                        .connect_with(
                            client.config.clone(),
                            "[::1]:9".parse().unwrap(),
                            "localhost",
                        )
                        .unwrap();
                    assert_eq!(built.load(Ordering::SeqCst), usize::from(!overridden));
                    drop(connecting);
                    client.endpoint.close(0u32.into(), b"done");
                }
            }
        }
    }

    #[tokio::test]
    async fn default_congestion_control_removes_only_the_overlay() {
        let (transport, built) = custom_transport();
        let client = builder()
            .with_transport_config(transport)
            .with_congestion_control(CongestionControl::LowLatency)
            .with_congestion_control(CongestionControl::Default)
            .dangerous()
            .with_no_certificate_verification()
            .unwrap();
        assert_transport(&client.config, true);
        let connecting = client
            .endpoint
            .connect_with(
                client.config.clone(),
                "[::1]:9".parse().unwrap(),
                "localhost",
            )
            .unwrap();
        assert_eq!(built.load(Ordering::SeqCst), 1);
        drop(connecting);
        client.endpoint.close(0u32.into(), b"done");
    }
}
