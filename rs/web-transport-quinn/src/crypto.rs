//! Provider-neutral Quinn TLS over rustls, with rind for Retry and endpoint keys.
//!
//! Use [`crate::crypto::QuicClientConfig`] and [`crate::crypto::QuicServerConfig`] in place of Quinn's
//! `crypto::rustls` types, [`crate::crypto::endpoint_config`] with `quinn::Endpoint::new`, and
//! [`crate::crypto::server_config`] in place of `quinn::ServerConfig::with_crypto`.

use std::sync::Arc;

use rustls::crypto::hash::{self, HashAlgorithm};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use thiserror::Error;

mod session;
pub use session::{HandshakeData, QuicClientConfig, QuicServerConfig};
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
    /// The provider lacks the QUIC AES-128-GCM-SHA256 initial suite.
    #[error("provider has no QUIC AES-128-GCM-SHA256 initial cipher suite")]
    NoInitialCipherSuite,
    /// The TLS configuration is invalid for QUIC.
    #[error("invalid QUIC TLS configuration: {0}")]
    Tls(#[from] rustls::Error),
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

/// Creates endpoint configuration with a fresh rind stateless-reset key.
pub fn endpoint_config() -> Result<quinn::EndpointConfig, ConfigError> {
    check_cpu()?;
    let mut secret = [0; 64];
    rind::rand::fill(&mut secret).map_err(|_| ConfigError::Random)?;
    Ok(quinn::EndpointConfig::new(Arc::new(ResetKey::new(
        &secret,
    )?)))
}

/// Creates server configuration with a fresh rind handshake-token key.
pub fn server_config(
    crypto: Arc<dyn quinn::crypto::ServerConfig>,
) -> Result<quinn::ServerConfig, ConfigError> {
    check_cpu()?;
    let mut secret = [0; 64];
    rind::rand::fill(&mut secret).map_err(|_| ConfigError::Random)?;
    Ok(quinn::ServerConfig::new(
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
        const CHILD: &str = "WEB_TRANSPORT_QUINN_PROVIDER_TEST_CHILD";
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
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("only this test installs a provider");
        let provider = CryptoProvider::get_default().unwrap().clone();
        assert!(Arc::ptr_eq(&default_provider().unwrap(), &provider));
        let mut roots = rustls::RootCertStore::empty();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        roots.add(cert.cert.der().clone()).unwrap();
        let client_tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let server_tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert.der().clone()], key.clone_key().into())
            .unwrap();
        let server = Arc::new(QuicServerConfig::try_from(server_tls.clone()).unwrap());
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
        assert!(matches!(
            QuicClientConfig::try_from(client_tls),
            Err(ConfigError::UnsupportedCpu(_))
        ));
        assert!(matches!(
            QuicServerConfig::try_from(server_tls),
            Err(ConfigError::UnsupportedCpu(_))
        ));
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
