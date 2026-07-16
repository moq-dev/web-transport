//! QUIC transport controls: keep-alive and the GSO opt-out.
//!
//! The load-bearing assertion for keep-alive is the negative one: the same
//! connection *without* it must actually die of the idle timeout, otherwise the
//! positive test would pass no matter what the driver does.

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

/// Short enough to keep the test quick, long enough to survive a loaded CI
/// machine stalling the driver task between ticks.
const IDLE_TIMEOUT: Duration = Duration::from_secs(1);
const KEEP_ALIVE: Duration = Duration::from_millis(200);

/// Long enough that an un-kept connection is certainly gone.
const IDLE_WAIT: Duration = Duration::from_secs(3);

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
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

fn idle_settings() -> Settings {
    let mut settings = Settings::default();
    settings.max_idle_timeout = Some(IDLE_TIMEOUT);
    settings
}

/// Spawn a server that accepts one session and holds it open without sending
/// anything, so the only traffic on the wire is whatever keep-alive produces.
async fn spawn_server(gso: bool) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let (chain, key) = make_self_signed()?;

    let bind: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_settings(idle_settings())
        .with_gso(gso)
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

fn url_for(addr: SocketAddr) -> Result<Url> {
    Ok(Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?)
}

fn client() -> ClientBuilder {
    // The cert is self-signed, and the point here is transport behavior rather
    // than verification, which verify.rs already covers.
    let mut settings = idle_settings();
    settings.verify_peer = false;

    ClientBuilder::default().with_settings(settings)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_outlives_idle_timeout() -> Result<()> {
    init_tracing();

    let (addr, server) = spawn_server(true).await?;

    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .with_keep_alive(KEEP_ALIVE)
        .connect(url_for(addr)?)
        .await?
        .established()
        .await?;

    // Nothing is sent on the session, so only the keep-alive PINGs can hold the
    // idle timer open on either end.
    if let Ok(err) = tokio::time::timeout(IDLE_WAIT, session.closed()).await {
        anyhow::bail!("keep-alive connection closed after {IDLE_WAIT:?}: {err}");
    }

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

/// The control for `keep_alive_outlives_idle_timeout`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timeout_without_keep_alive() -> Result<()> {
    init_tracing();

    let (addr, server) = spawn_server(true).await?;

    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url_for(addr)?)
        .await?
        .established()
        .await?;

    tokio::time::timeout(IDLE_WAIT, session.closed())
        .await
        .map_err(|_| {
            anyhow::anyhow!("idle connection survived {IDLE_WAIT:?} with no keep-alive")
        })?;

    server.abort();
    Ok(())
}

/// GSO is Linux-only, so elsewhere this just pins that the option is accepted
/// rather than rejected at initialization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_without_gso() -> Result<()> {
    init_tracing();

    let (addr, server) = spawn_server(false).await?;

    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .with_gso(false)
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed with GSO disabled")?;

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}
