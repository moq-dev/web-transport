use rcgen::{CertificateParams, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use web_transport_quinn::crypto;

/// A self-signed certificate for use in tests, along with its SHA-256 fingerprint.
pub struct TestCert {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    /// 32 raw SHA-256 bytes.
    pub fingerprint: Vec<u8>,
    /// Hex-encoded fingerprint for logging.
    pub fingerprint_hex: String,
}

/// Generate a self-signed EC P-256 certificate valid for 10 days.
///
/// The certificate has SANs for `localhost` and `127.0.0.1`.
pub fn generate() -> TestCert {
    let key_pair = KeyPair::generate().expect("failed to generate key pair");

    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress("127.0.0.1".parse().unwrap()),
    ];
    // Chrome requires serverCertificateHashes certs to be valid for at most 14 days.
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::days(10);

    let cert = params
        .self_signed(&key_pair)
        .expect("failed to self-sign certificate");

    let cert_der = CertificateDer::from(cert);
    let key_der = PrivateKeyDer::from(key_pair);

    // Compute fingerprint using the same function as the server-side code.
    let provider = crypto::default_provider();
    let hash = crypto::sha256(&provider, &cert_der);
    let fingerprint = hash.as_ref().to_vec();
    let fingerprint_hex = fingerprint
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    tracing::debug!(fingerprint = %fingerprint_hex, "generated test certificate");

    TestCert {
        chain: vec![cert_der],
        key: key_der,
        fingerprint,
        fingerprint_hex,
    }
}
