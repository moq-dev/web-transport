//! Certificate fixtures shared by the TLS tests.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// A CA root plus a server leaf it issued.
pub struct CaChain {
    pub root: CertificateDer<'static>,
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

/// Issue a fresh CA and a server leaf for `sans`, named after the first one.
pub fn ca_chain(sans: &[&str]) -> Result<CaChain> {
    let ca_key = KeyPair::generate().context("ca key")?;
    let mut ca_params = CertificateParams::new(Vec::new()).context("ca params")?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "web-transport test CA");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).context("self-sign ca")?;

    let leaf_key = KeyPair::generate().context("leaf key")?;
    let sans: Vec<String> = sans.iter().map(|s| s.to_string()).collect();
    let mut leaf_params = CertificateParams::new(sans.clone()).context("leaf params")?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, sans.first().context("no SAN")?);
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let ca_issuer = Issuer::from_params(&ca_params, &ca_key);
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_issuer)
        .context("sign leaf")?;

    Ok(CaChain {
        root: CertificateDer::from(ca_cert.der().to_vec()),
        chain: vec![CertificateDer::from(leaf_cert.der().to_vec())],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(&leaf_key))),
    })
}
