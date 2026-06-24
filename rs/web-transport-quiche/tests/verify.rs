//! Server-certificate verification: SHA-256 hash pinning (the browser's
//! `serverCertificateHashes` equivalent) and explicit root certificates.
//!
//! The security-critical assertion is the negative one: a wrong hash must
//! *fail* the handshake, not silently connect.

use std::{
    net::{Ipv6Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedKey, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder};

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&key_pair);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

/// A CA certificate plus a leaf signed by it. Returns `(ca_root, leaf_chain, leaf_key)`.
#[allow(clippy::type_complexity)]
fn make_ca_chain() -> Result<(
    CertificateDer<'static>,
    Vec<CertificateDer<'static>>,
    PrivateKeyDer<'static>,
)> {
    let ca_key = KeyPair::generate().context("ca key")?;
    let mut ca_params = CertificateParams::new(Vec::new()).context("ca params")?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "web-transport test CA");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).context("self-sign ca")?;

    let leaf_key = KeyPair::generate().context("leaf key")?;
    let mut leaf_params = CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
        .context("leaf params")?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .context("sign leaf")?;

    let ca_der = CertificateDer::from(ca_cert.der().to_vec());
    let leaf_der = CertificateDer::from(leaf_cert.der().to_vec());
    let leaf_key_der =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(&leaf_key)));

    Ok((ca_der, vec![leaf_der], leaf_key_der))
}

fn cert_sha256(chain: &[CertificateDer<'static>]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(chain[0].as_ref());
    hasher.finalize().into()
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

/// Bind a server with the given cert and spawn a task that accepts (and holds)
/// a single session. Returns the bound address and the task handle.
async fn spawn_server(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let bind: SocketAddr = (Ipv6Addr::LOCALHOST, 0).into();
    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let handle = tokio::spawn(async move {
        if let Some(request) = server.accept().await {
            if let Ok(session) = request.ok().await {
                let _ = session.closed().await;
            }
        }
    });

    Ok((addr, handle))
}

// Connect by hostname so root-based verification sees a matching DNS SAN.
// `localhost` resolves to `::1` first on the test hosts, matching the IPv6 bind.
fn url_for(addr: SocketAddr) -> Result<Url> {
    Ok(Url::parse(&format!("https://localhost:{}/", addr.port()))?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cert_hash_accept() -> Result<()> {
    init_tracing();

    let (chain, key) = make_self_signed()?;
    let hash = cert_sha256(&chain);
    let (addr, server) = spawn_server(chain, key).await?;

    let session = ClientBuilder::default()
        .with_bind((Ipv6Addr::LOCALHOST, 0))?
        .with_server_certificate_hashes(vec![hash])
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed with the correct hash")?;

    // Stats should be populated once a path is established (covers the live
    // stats plumbing, not just `StatsUnavailable`).
    let stats = session.stats();
    assert!(
        stats.bytes_sent > 0 && stats.packets_sent > 0,
        "expected non-zero send counters, got {stats:?}"
    );
    assert!(
        stats.rtt.is_some(),
        "expected an RTT estimate once a path is established, got {stats:?}"
    );

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cert_hash_reject() -> Result<()> {
    init_tracing();

    let (chain, key) = make_self_signed()?;
    let (addr, server) = spawn_server(chain, key).await?;

    // A hash that does not match any certificate.
    let wrong = [0xAB; 32];
    let url = url_for(addr)?;

    let result = tokio::time::timeout(Duration::from_secs(5), async move {
        ClientBuilder::default()
            .with_bind((Ipv6Addr::LOCALHOST, 0))?
            .with_server_certificate_hashes(vec![wrong])
            .connect(url)
            .await?
            .established()
            .await
    })
    .await
    .context("handshake neither succeeded nor failed within the timeout")?;

    assert!(
        result.is_err(),
        "handshake must fail when the certificate hash does not match"
    );

    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_roots_accept() -> Result<()> {
    init_tracing();

    let (ca_root, chain, key) = make_ca_chain()?;
    let (addr, server) = spawn_server(chain, key).await?;

    let session = ClientBuilder::default()
        .with_bind((Ipv6Addr::LOCALHOST, 0))?
        .with_root_certificates(vec![ca_root])
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed when the cert is a trusted root")?;

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}
