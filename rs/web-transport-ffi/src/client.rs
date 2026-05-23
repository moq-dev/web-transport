//! WebTransport client wrapped for UniFFI.
//!
//! Mirrors `rs/web-transport-python/src/client.rs` with PyO3 replaced by
//! `#[uniffi::export]` and the `ring` crypto provider replaced by either
//! `aws-lc-rs` (default) or `ring` (feature-gated).

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::CertificateDer;

use crate::error::{map_client_error, WebTransportError};
use crate::ffi::RUNTIME;
use crate::session::Session;

/// Congestion control algorithm exposed via UniFFI.
#[derive(Debug, Clone, Copy, Default, uniffi::Enum)]
pub enum CongestionControl {
    #[default]
    Default,
    Throughput,
    LowLatency,
}

/// Configuration record for a [`Client`].
///
/// All fields default to sensible values; `server_certificate_hashes` and
/// `no_cert_verification` are mutually exclusive.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ClientConfig {
    #[uniffi(default = None)]
    pub server_certificate_hashes: Option<Vec<Vec<u8>>>,
    #[uniffi(default = false)]
    pub no_cert_verification: bool,
    pub congestion_control: CongestionControl,
    #[uniffi(default = Some(30.0))]
    pub max_idle_timeout_secs: Option<f64>,
    #[uniffi(default = None)]
    pub keep_alive_interval_secs: Option<f64>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_certificate_hashes: None,
            no_cert_verification: false,
            congestion_control: CongestionControl::Default,
            max_idle_timeout_secs: Some(30.0),
            keep_alive_interval_secs: None,
        }
    }
}

#[derive(uniffi::Object)]
pub struct Client {
    inner: web_transport_quinn::Client,
    endpoint: quinn::Endpoint,
}

#[uniffi::export]
impl Client {
    /// Build a new WebTransport client.
    #[uniffi::constructor]
    pub fn new(config: ClientConfig) -> Result<Arc<Self>, WebTransportError> {
        if config.no_cert_verification && config.server_certificate_hashes.is_some() {
            return Err(WebTransportError::invalid(
                "no_cert_verification and server_certificate_hashes are mutually exclusive",
            ));
        }

        let provider = Arc::new(default_crypto_provider());

        let tls_builder = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| WebTransportError::invalid(format!("TLS config: {e}")))?;

        let mut tls_config = if config.no_cert_verification {
            let noop = NoCertificateVerification(provider.clone());
            tls_builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(noop))
                .with_no_client_auth()
        } else if let Some(hashes) = config.server_certificate_hashes.clone() {
            let fingerprints = Arc::new(ServerFingerprints {
                provider: provider.clone(),
                fingerprints: hashes,
            });
            tls_builder
                .dangerous()
                .with_custom_certificate_verifier(fingerprints)
                .with_no_client_auth()
        } else {
            let mut roots = rustls::RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            // Errors loading individual certs are non-fatal; we still want
            // whatever the platform yielded.
            for cert in native.certs {
                let _ = roots.add(cert);
            }
            tls_builder
                .with_root_certificates(roots)
                .with_no_client_auth()
        };

        tls_config.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];

        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(
            config
                .max_idle_timeout_secs
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| WebTransportError::invalid("invalid max_idle_timeout_secs"))?
                .map(quinn::IdleTimeout::try_from)
                .transpose()
                .map_err(|e| WebTransportError::invalid(format!("invalid idle timeout: {e}")))?,
        );
        transport.keep_alive_interval(
            config
                .keep_alive_interval_secs
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| WebTransportError::invalid("invalid keep_alive_interval_secs"))?,
        );
        match config.congestion_control {
            CongestionControl::Default => {}
            CongestionControl::Throughput => {
                transport.congestion_controller_factory(Arc::new(
                    quinn::congestion::CubicConfig::default(),
                ));
            }
            CongestionControl::LowLatency => {
                transport.congestion_controller_factory(Arc::new(
                    quinn::congestion::BbrConfig::default(),
                ));
            }
        }

        let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
            .map_err(|e| WebTransportError::invalid(format!("QUIC config: {e}")))?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
        client_config.transport_config(transport.into());

        let _guard = RUNTIME.enter();
        let endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())
            .or_else(|_| quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()))
            .map_err(|e| WebTransportError::Io(format!("failed to create endpoint: {e}")))?;

        let client = web_transport_quinn::Client::new(endpoint.clone(), client_config);

        Ok(Arc::new(Self {
            inner: client,
            endpoint,
        }))
    }

    /// Open a WebTransport session to `url`.
    pub async fn connect(&self, url: String) -> Result<Arc<Session>, WebTransportError> {
        let client = self.inner.clone();
        let parsed: url::Url = url
            .parse()
            .map_err(|e| WebTransportError::invalid(format!("invalid URL: {e}")))?;

        let handle =
            RUNTIME.spawn(async move { client.connect(parsed).await.map_err(map_client_error) });

        let session = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("connect task: {e}")))??;

        Ok(Session::new(session))
    }

    /// Close the endpoint and all connections.
    #[uniffi::method(default(code = 0, reason = ""))]
    pub fn close(&self, code: u64, reason: String) -> Result<(), WebTransportError> {
        let var_code = quinn::VarInt::from_u64(code)
            .map_err(|_| WebTransportError::invalid("code must be < 2^62"))?;
        let _guard = RUNTIME.enter();
        self.endpoint.close(var_code, reason.as_bytes());
        Ok(())
    }

    /// Wait until all connections have shut down cleanly.
    pub async fn wait_closed(&self) {
        let endpoint = self.endpoint.clone();
        let handle = RUNTIME.spawn(async move {
            endpoint.wait_idle().await;
        });
        let _ = handle.await;
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _guard = RUNTIME.enter();
        self.endpoint.close(quinn::VarInt::from_u32(0), b"");
    }
}

// ---------------------------------------------------------------------------
// Crypto provider selection (matches feature gates in Cargo.toml).
// ---------------------------------------------------------------------------

fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "aws-lc-rs")]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }
    #[cfg(all(feature = "ring", not(feature = "aws-lc-rs")))]
    {
        rustls::crypto::ring::default_provider()
    }
    #[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
    {
        compile_error!("web-transport-ffi requires one of: aws-lc-rs, ring");
    }
}

// ---------------------------------------------------------------------------
// Certificate verifiers — preserved from web-transport-python.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ServerFingerprints {
    provider: Arc<rustls::crypto::CryptoProvider>,
    fingerprints: Vec<Vec<u8>>,
}

impl ServerCertVerifier for ServerFingerprints {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let cert_hash = sha256(&self.provider, end_entity)?;
        if self.fingerprints.iter().any(|fp| fp == cert_hash.as_ref()) {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }
        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ))
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
            &self.provider.signature_verification_algorithms,
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
struct NoCertificateVerification(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for NoCertificateVerification {
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

fn sha256(
    provider: &Arc<rustls::crypto::CryptoProvider>,
    cert: &CertificateDer<'_>,
) -> Result<rustls::crypto::hash::Output, rustls::Error> {
    let hash_provider = provider
        .cipher_suites
        .iter()
        .find_map(|suite| {
            let hp = suite.tls13()?.common.hash_provider;
            if hp.algorithm() == rustls::crypto::hash::HashAlgorithm::SHA256 {
                Some(hp)
            } else {
                None
            }
        })
        .ok_or_else(|| rustls::Error::General("crypto provider missing SHA-256".into()))?;
    Ok(hash_provider.hash(cert))
}

// ---------------------------------------------------------------------------
// Helper used by server.rs for TLS configuration.
// ---------------------------------------------------------------------------

pub(crate) fn build_server_tls(
    certificate_chain: Vec<Vec<u8>>,
    private_key: Vec<u8>,
) -> Result<rustls::ServerConfig, WebTransportError> {
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> = certificate_chain
        .into_iter()
        .map(rustls::pki_types::CertificateDer::from)
        .collect();
    let key = rustls::pki_types::PrivateKeyDer::try_from(private_key)
        .map_err(|e| WebTransportError::invalid(format!("invalid private key: {e}")))?;

    let provider = default_crypto_provider();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| WebTransportError::invalid(format!("TLS config: {e}")))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| WebTransportError::invalid(format!("certificate: {e}")))?;
    tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
    Ok(tls)
}
