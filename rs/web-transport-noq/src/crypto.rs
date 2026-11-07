//! Rind TLS and endpoint keys for Noq, with installed rustls provider precedence.
//!
//! Pass [`initial_suite`] to Noq's `QuicClientConfig::with_initial` and
//! `QuicServerConfig::with_initial`, [`endpoint_config`] to `noq::Endpoint::new`,
//! and use [`server_config`] instead of `noq::ServerConfig::with_crypto`.

use std::sync::Arc;

use rustls::crypto::hash::{self, HashAlgorithm};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use thiserror::Error;

mod keys;
pub use keys::{ResetKey, TokenKey};

/// A shared reference to a crypto provider.
pub type Provider = Arc<CryptoProvider>;

/// A CPU feature required by rind is unavailable.
#[derive(Clone, Debug, Error)]
#[error("unsupported CPU: {0}")]
pub struct UnsupportedCpu(pub(crate) String);

/// An error constructing QUIC cryptography.
#[derive(Clone, Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A CPU feature required by rind is unavailable.
    #[error(transparent)]
    UnsupportedCpu(#[from] UnsupportedCpu),
    /// The system random source failed.
    #[error("system random source failed")]
    Random,
}

/// Checks CPU support before using rind, including with an installed TLS provider.
fn check_cpu() -> Result<(), UnsupportedCpu> {
    #[cfg(test)]
    if REFUSE_CPU.get() {
        return Err(UnsupportedCpu("simulated missing CPU feature".into()));
    }
    rind::cpu::check().map_err(|e| UnsupportedCpu(e.to_string()))
}

/// Returns the installed process provider, otherwise rind, after checking CPU support.
pub fn default_provider() -> Result<Provider, UnsupportedCpu> {
    check_cpu()?;
    if let Some(provider) = CryptoProvider::get_default().cloned() {
        return Ok(provider);
    }
    rind_rustls::default_provider()
        .map(Arc::new)
        .map_err(|e| UnsupportedCpu(e.to_string()))
}

/// Returns rind's QUIC AES-128-GCM-SHA256 initial suite after checking CPU support.
pub fn initial_suite() -> Result<rustls::quic::Suite, UnsupportedCpu> {
    check_cpu()?;
    Ok(rind_rustls::cipher_suite::TLS13_AES_128_GCM_SHA256
        .tls13()
        .expect("TLS 1.3 suite")
        .quic_suite()
        .expect("QUIC suite"))
}

/// Creates endpoint configuration with a fresh rind stateless-reset key.
pub fn endpoint_config() -> Result<noq::EndpointConfig, ConfigError> {
    check_cpu()?;
    let mut secret = [0; 64];
    rind::rand::fill(&mut secret).map_err(|_| ConfigError::Random)?;
    Ok(noq::EndpointConfig::new(Arc::new(ResetKey::new(&secret)?)))
}

/// Creates server configuration with a fresh rind handshake-token key.
pub fn server_config(
    crypto: Arc<dyn noq::crypto::ServerConfig>,
) -> Result<noq::ServerConfig, ConfigError> {
    check_cpu()?;
    let mut secret = [0; 64];
    rind::rand::fill(&mut secret).map_err(|_| ConfigError::Random)?;
    Ok(noq::ServerConfig::new(
        crypto,
        Arc::new(TokenKey::new(&secret)?),
    ))
}

/// Computes the SHA-256 hash of a certificate using the provided crypto provider.
pub fn sha256(
    provider: &Provider,
    cert: &CertificateDer<'_>,
) -> Result<hash::Output, rustls::Error> {
    let hash_provider = provider.cipher_suites.iter().find_map(|suite| {
        let hash_provider = suite.tls13()?.common.hash_provider;
        (hash_provider.algorithm() == HashAlgorithm::SHA256).then_some(hash_provider)
    });
    Ok(hash_provider
        .ok_or_else(|| rustls::Error::General("provider has no SHA-256 hash".into()))?
        .hash(cert))
}

#[cfg(test)]
thread_local! {
    static REFUSE_CPU: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_provider_and_cpu_refusal() {
        // Isolate the process-wide provider from every other test in this binary.
        const CHILD: &str = "WEB_TRANSPORT_NOQ_PROVIDER_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "crypto::tests::installed_provider_and_cpu_refusal",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("only this test installs a provider");
        let provider = CryptoProvider::get_default().unwrap().clone();
        assert!(Arc::ptr_eq(&default_provider().unwrap(), &provider));
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let server_tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert.der().clone()], key.clone_key().into())
            .unwrap();
        let server = Arc::new(
            noq::crypto::rustls::QuicServerConfig::with_initial(
                Arc::new(server_tls),
                initial_suite().unwrap(),
            )
            .unwrap(),
        );
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                REFUSE_CPU.set(false);
            }
        }
        let _reset = Reset;
        REFUSE_CPU.set(true);
        assert!(
            default_provider().is_err(),
            "installed provider must not bypass rind's CPU check"
        );
        assert!(endpoint_config().is_err());
        assert!(server_config(server).is_err());
        assert!(ResetKey::new(&[0; 64]).is_err());
        assert!(TokenKey::new(&[0; 64]).is_err());
        assert!(initial_suite().is_err());
        assert!(matches!(
            crate::ClientBuilder::new().with_system_roots(),
            Err(crate::ClientError::UnsupportedCpu(_))
        ));
        assert!(matches!(
            crate::ClientBuilder::new().with_server_certificates(vec![]),
            Err(crate::ClientError::UnsupportedCpu(_))
        ));
        assert!(matches!(
            crate::ClientBuilder::new().with_server_certificate_hashes(vec![]),
            Err(crate::ClientError::UnsupportedCpu(_))
        ));
        assert!(matches!(
            crate::ClientBuilder::new()
                .dangerous()
                .with_no_certificate_verification(),
            Err(crate::ClientError::UnsupportedCpu(_))
        ));
        assert!(matches!(
            crate::ServerBuilder::new().with_certificate(vec![], key.into()),
            Err(crate::ServerError::UnsupportedCpu(_))
        ));
    }
}
