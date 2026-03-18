use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::CertificateDer;

use crate::errors;
use crate::runtime;
use crate::session::Session;

#[pyclass]
pub struct Client {
    inner: web_transport_quinn::Client,
    endpoint: quinn::Endpoint,
}

#[pymethods]
impl Client {
    #[new]
    #[pyo3(signature = (*, server_certificate_hashes=None, no_cert_verification=false, congestion_control="default", max_idle_timeout=Some(30.0), keep_alive_interval=None))]
    fn new(
        server_certificate_hashes: Option<Vec<Vec<u8>>>,
        no_cert_verification: bool,
        congestion_control: &str,
        max_idle_timeout: Option<f64>,
        keep_alive_interval: Option<f64>,
    ) -> PyResult<Self> {
        // Crypto provider — matches web_transport_quinn::crypto::default_provider()
        // with the "ring" feature enabled.
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        // TLS 1.3 only, matching ClientBuilder::builder()
        let tls_builder = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| PyValueError::new_err(format!("TLS config error: {e}")))?;

        // Certificate verification — three modes matching ClientBuilder
        let mut tls_config = if no_cert_verification {
            // Matches DangerousClientBuilder::with_no_certificate_verification()
            let noop = NoCertificateVerification(provider.clone());
            tls_builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(noop))
                .with_no_client_auth()
        } else if let Some(hashes) = server_certificate_hashes {
            // Matches ClientBuilder::with_server_certificate_hashes()
            let fingerprints = Arc::new(ServerFingerprints {
                provider: provider.clone(),
                fingerprints: hashes,
            });
            tls_builder
                .dangerous()
                .with_custom_certificate_verifier(fingerprints)
                .with_no_client_auth()
        } else {
            // Matches ClientBuilder::with_system_roots()
            let mut roots = rustls::RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            if !native.errors.is_empty() {
                eprintln!(
                    "warning: encountered errors loading native certificates: {:?}",
                    native.errors
                );
            }
            for cert in native.certs {
                let _ = roots.add(cert);
            }
            tls_builder
                .with_root_certificates(roots)
                .with_no_client_auth()
        };

        // ALPN "h3" — matches ClientBuilder::build()
        tls_config.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];

        // Build transport config with all user settings
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(
            max_idle_timeout
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| PyValueError::new_err("invalid max_idle_timeout"))?
                .map(quinn::IdleTimeout::try_from)
                .transpose()
                .map_err(|e| PyValueError::new_err(format!("invalid idle timeout: {e}")))?,
        );
        transport.keep_alive_interval(
            keep_alive_interval
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| PyValueError::new_err("invalid keep_alive_interval"))?,
        );

        // Congestion control — matches ClientBuilder::with_congestion_control()
        let congestion_controller: Option<
            Arc<dyn quinn::congestion::ControllerFactory + Send + Sync + 'static>,
        > = match congestion_control {
            "default" => None,
            "throughput" => Some(Arc::new(quinn::congestion::CubicConfig::default())),
            "low_latency" => Some(Arc::new(quinn::congestion::BbrConfig::default())),
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown congestion control: {other}"
                )));
            }
        };

        if let Some(cc) = congestion_controller {
            transport.congestion_controller_factory(cc.clone());
        }

        // Build quinn client config
        let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
            .map_err(|e| PyValueError::new_err(format!("QUIC config error: {e}")))?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
        client_config.transport_config(transport.into());

        // Create endpoint bound to [::]:0 — matches ClientBuilder::build()
        let _guard = runtime::get_runtime().enter();
        let endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())
            .map_err(|e| PyValueError::new_err(format!("failed to create endpoint: {e}")))?;

        // Build the upstream Client via its public constructor
        let client = web_transport_quinn::Client::new(endpoint.clone(), client_config);

        Ok(Self {
            inner: client,
            endpoint,
        })
    }

    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let obj: Py<PyAny> = slf.into_pyobject(py)?.into_any().unbind();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(obj) })
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = self.endpoint.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            endpoint.close(quinn::VarInt::from_u32(0), b"");
            endpoint.wait_idle().await;
            Ok(())
        })
    }

    /// Close all connections immediately.
    #[pyo3(signature = (code=0, reason=""))]
    fn close(&self, code: u64, reason: &str) -> PyResult<()> {
        let var_code = quinn::VarInt::from_u64(code)
            .map_err(|_| PyValueError::new_err("code must be less than 2**62"))?;
        self.endpoint.close(var_code, reason.as_bytes());
        Ok(())
    }

    /// Wait for all connections to be cleanly shut down.
    fn wait_closed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = self.endpoint.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            endpoint.wait_idle().await;
            Ok(())
        })
    }

    /// Open a WebTransport session to the given URL.
    fn connect<'py>(&self, py: Python<'py>, url: String) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();

        let parsed_url: url::Url = url
            .parse()
            .map_err(|e| PyValueError::new_err(format!("invalid URL: {e}")))?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let session = client
                .connect(parsed_url)
                .await
                .map_err(errors::map_client_error)?;
            Ok(Session::new(session))
        })
    }
}

// ---------------------------------------------------------------------------
// Certificate verifiers (matching web_transport_quinn's implementations)
// ---------------------------------------------------------------------------

/// Verifies server certificates by comparing SHA-256 fingerprints.
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

/// Disables all certificate verification. Matches
/// `web_transport_quinn::client::NoCertificateVerification`.
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

// ---------------------------------------------------------------------------
// Crypto helpers (matching web_transport_quinn::crypto)
// ---------------------------------------------------------------------------

/// Compute SHA-256 hash of a certificate.
fn sha256(
    provider: &Arc<rustls::crypto::CryptoProvider>,
    cert: &CertificateDer<'_>,
) -> Result<rustls::crypto::hash::Output, rustls::Error> {
    let hash_provider = provider
        .cipher_suites
        .iter()
        .find_map(|suite| {
            let hash_provider = suite.tls13()?.common.hash_provider;
            if hash_provider.algorithm() == rustls::crypto::hash::HashAlgorithm::SHA256 {
                Some(hash_provider)
            } else {
                None
            }
        })
        .ok_or_else(|| rustls::Error::General("crypto provider does not support SHA-256".into()))?;
    Ok(hash_provider.hash(cert))
}
