//! Dialing by address literal.
//!
//! `Url::host()` renders an IPv6 host in URL form, bracketed, so handing it
//! straight to a resolver fails. Hostname URLs never exercise that path, which
//! is why it went unnoticed.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "::1".into(),
        "127.0.0.1".into(),
    ])
    .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
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

async fn spawn_server(bind: SocketAddr) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let (chain, key) = make_self_signed()?;

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

fn client() -> ClientBuilder {
    // The certificate is self-signed and the subject here is address handling,
    // which verify.rs already covers from the other side.
    let mut settings = Settings::default();
    settings.verify_peer = false;

    ClientBuilder::default().with_settings(settings)
}

/// `https://[::1]:port/` must reach the server. Before the fix the bracketed
/// form reached the resolver verbatim and failed to resolve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_ipv6_literal_url() -> Result<()> {
    init_tracing();

    let (addr, server) = spawn_server((Ipv6Addr::LOCALHOST, 0).into()).await?;
    let url = Url::parse(&format!("https://[::1]:{}/", addr.port()))?;

    let session = client()
        .with_bind((Ipv6Addr::LOCALHOST, 0))?
        .connect(url)
        .await
        .context("an IPv6 literal URL should resolve to the address it names")?
        .established()
        .await?;

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

/// The IPv4 counterpart, which already worked: `Host`'s IPv4 form needs no
/// unwrapping. It guards against a "fix" that breaks the common case.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_ipv4_literal_url() -> Result<()> {
    init_tracing();

    let (addr, server) = spawn_server((Ipv4Addr::LOCALHOST, 0).into()).await?;
    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;

    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}
