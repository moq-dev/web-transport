//! The TLS server name: what the server reports, and overriding it on the client
//! independently of the dial target.
//!
//! The override tests use a certificate issued for a name that never resolves, so
//! the only way the handshake can succeed is if the override reached both SNI and
//! the hostname check — and the only way it can fail is if it didn't.

use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{
    ez::{CertResolver, CertifiedKey},
    ClientBuilder, ServerBuilder,
};

/// A name the client can dial, because it resolves to loopback.
const DIAL_NAME: &str = "localhost";

/// A name the client cannot dial: it only ever reaches the wire as an override.
const OVERRIDE_NAME: &str = "override.invalid";

/// A CA plus a leaf issued for `sans`. Returns `(ca_root, chain, key)`.
#[allow(clippy::type_complexity)]
fn make_ca_chain(
    sans: &[&str],
) -> Result<(
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

    Ok((
        CertificateDer::from(ca_cert.der().to_vec()),
        vec![CertificateDer::from(leaf_cert.der().to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(&leaf_key))),
    ))
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

/// The SNI the server reported for the one session it accepted.
type ServerName = tokio::sync::oneshot::Receiver<Option<String>>;

/// Bind a server that accepts a single session and reports the SNI it saw.
async fn spawn_server(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>, ServerName)> {
    // Bind to whatever `localhost` resolves to first. The client connects to the
    // same name, so both agree on the address family regardless of v4/v6 ordering.
    let bind = (DIAL_NAME, 0)
        .to_socket_addrs()?
        .next()
        .context("localhost did not resolve")?;

    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let (tx, rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        if let Some(request) = server.accept().await {
            let _ = tx.send(request.conn().server_name());

            if let Ok(session) = request.ok().await {
                let _ = session.closed().await;
            }
        }
    });

    Ok((addr, handle, rx))
}

fn url_for(addr: SocketAddr) -> Result<Url> {
    Ok(Url::parse(&format!(
        "https://{DIAL_NAME}:{}/",
        addr.port()
    ))?)
}

/// Loopback bind address matching the family of `addr`, for the client socket.
fn loopback_for(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V4(_) => (Ipv4Addr::LOCALHOST, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::LOCALHOST, 0).into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_name_reported() -> Result<()> {
    init_tracing();

    let (ca_root, chain, key) = make_ca_chain(&[DIAL_NAME])?;
    let (addr, server, server_name) = spawn_server(chain, key).await?;

    let session = ClientBuilder::default()
        .with_bind(loopback_for(addr))?
        .with_root_certificates(vec![ca_root])
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed")?;

    assert_eq!(server_name.await?.as_deref(), Some(DIAL_NAME));

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_name_override_accept() -> Result<()> {
    init_tracing();

    let (ca_root, chain, key) = make_ca_chain(&[OVERRIDE_NAME])?;
    let (addr, server, server_name) = spawn_server(chain, key).await?;

    let session = ClientBuilder::default()
        .with_bind(loopback_for(addr))?
        .with_root_certificates(vec![ca_root])
        .with_server_name(OVERRIDE_NAME)
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should verify against the overridden name")?;

    // The override must reach the wire as SNI, not just the local hostname check.
    assert_eq!(server_name.await?.as_deref(), Some(OVERRIDE_NAME));

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_name_default_reject() -> Result<()> {
    init_tracing();

    // Without the override the dial target is the name checked, and this
    // certificate isn't valid for it. If this ever passes, the hostname check
    // isn't happening at all and the test above proves nothing.
    let (ca_root, chain, key) = make_ca_chain(&[OVERRIDE_NAME])?;
    let (addr, server, _server_name) = spawn_server(chain, key).await?;

    let url = url_for(addr)?;
    let client_bind = loopback_for(addr);

    let result = tokio::time::timeout(Duration::from_secs(5), async move {
        ClientBuilder::default()
            .with_bind(client_bind)?
            .with_root_certificates(vec![ca_root])
            .connect(url)
            .await?
            .established()
            .await
    })
    .await
    .context("handshake neither succeeded nor failed within the timeout")?;

    assert!(
        result.is_err(),
        "handshake must fail when the certificate isn't valid for the dialed host"
    );

    server.abort();
    Ok(())
}

/// Serves one certificate and records the SNI it was asked to resolve.
struct RecordingResolver {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    seen: Mutex<Vec<Option<String>>>,
}

impl CertResolver for RecordingResolver {
    fn resolve(&self, server_name: Option<&str>) -> Option<CertifiedKey> {
        self.seen
            .lock()
            .unwrap()
            .push(server_name.map(str::to_string));

        Some(CertifiedKey {
            chain: self.chain.clone(),
            key: self.key.clone_key(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_name_reaches_cert_resolver() -> Result<()> {
    init_tracing();

    // The resolver picks the certificate mid-handshake, so it's the one place the
    // server sees SNI *before* deciding anything.
    let (ca_root, chain, key) = make_ca_chain(&[DIAL_NAME])?;
    let resolver = Arc::new(RecordingResolver {
        chain,
        key,
        seen: Mutex::new(Vec::new()),
    });

    let bind = (DIAL_NAME, 0)
        .to_socket_addrs()?
        .next()
        .context("localhost did not resolve")?;
    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_cert_resolver(resolver.clone())?;
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

    let session = ClientBuilder::default()
        .with_bind(loopback_for(addr))?
        .with_root_certificates(vec![ca_root])
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed")?;

    let seen = resolver.seen.lock().unwrap().clone();
    assert_eq!(seen, vec![Some(DIAL_NAME.to_string())]);

    session.close(0, "bye");
    session.closed().await;
    handle.abort();
    Ok(())
}
