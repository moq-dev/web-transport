//! Client-certificate authentication (mTLS) and the peer chain it exposes.
//!
//! The security-critical assertions are the negative ones: a client chaining to
//! an untrusted CA must *fail* the handshake, and a required certificate must
//! actually be required rather than quietly optional.

use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{ClientAuth, ClientBuilder, ServerBuilder, Settings};

/// A CA plus a leaf signed by it, for the given purpose and names.
struct Ca {
    root: CertificateDer<'static>,
    params: CertificateParams,
    key: KeyPair,
}

impl Ca {
    fn new(name: &str) -> Result<Self> {
        let key = KeyPair::generate().context("ca key")?;
        let mut params = CertificateParams::new(Vec::new()).context("ca params")?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name.push(DnType::CommonName, name);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let cert = params.self_signed(&key).context("self-sign ca")?;

        Ok(Self {
            root: CertificateDer::from(cert.der().to_vec()),
            params,
            key,
        })
    }

    /// Issue a leaf for the given SANs and extended key usage.
    fn issue(
        &self,
        sans: Vec<String>,
        common_name: &str,
        usage: ExtendedKeyUsagePurpose,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let key = KeyPair::generate().context("leaf key")?;
        let mut params = CertificateParams::new(sans).context("leaf params")?;
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.extended_key_usages = vec![usage];

        let issuer = Issuer::from_params(&self.params, &self.key);
        let cert = params.signed_by(&key, &issuer).context("sign leaf")?;

        let chain = vec![CertificateDer::from(cert.der().to_vec())];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(&key)));
        Ok((chain, key))
    }

    fn server_cert(&self) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        self.issue(
            vec!["localhost".into(), "127.0.0.1".into()],
            "localhost",
            ExtendedKeyUsagePurpose::ServerAuth,
        )
    }

    fn client_cert(
        &self,
        name: &str,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        self.issue(vec![name.into()], name, ExtendedKeyUsagePurpose::ClientAuth)
    }
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

/// The peer chain the server saw for the one session it accepted, or `None` if
/// the client presented no certificate.
type PeerCerts = tokio::sync::oneshot::Receiver<Option<Vec<CertificateDer<'static>>>>;

/// Bind a server that accepts a single session and reports the client's chain.
async fn spawn_server(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    client_auth: ClientAuth,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>, PeerCerts)> {
    // Bind to whatever `localhost` resolves to first, matching tests/verify.rs:
    // the client connects to the same name, so both agree on the address family.
    let bind = ("localhost", 0)
        .to_socket_addrs()?
        .next()
        .context("localhost did not resolve")?;

    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_client_auth(client_auth)
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let (tx, rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        if let Some(request) = server.accept().await {
            let certs = request.conn().peer_certificates();
            let _ = tx.send(certs);

            if let Ok(session) = request.ok().await {
                let _ = session.closed().await;
            }
        }
    });

    Ok((addr, handle, rx))
}

fn url_for(addr: SocketAddr) -> Result<Url> {
    Ok(Url::parse(&format!("https://localhost:{}/", addr.port()))?)
}

/// Loopback bind address matching the family of `addr`, for the client socket.
fn loopback_for(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V4(_) => (Ipv4Addr::LOCALHOST, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::LOCALHOST, 0).into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_accept() -> Result<()> {
    init_tracing();

    let ca = Ca::new("web-transport test CA")?;
    let (server_chain, server_key) = ca.server_cert()?;
    let (client_chain, client_key) = ca.client_cert("client.example")?;
    let client_leaf = client_chain[0].clone();

    let (addr, server, peer_certs) = spawn_server(
        server_chain,
        server_key,
        ClientAuth::Required(vec![ca.root.clone()]),
    )
    .await?;

    let session = ClientBuilder::default()
        .with_bind(loopback_for(addr))?
        .with_root_certificates(vec![ca.root])
        .with_single_cert(client_chain, client_key)
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed with a client cert from the trusted CA")?;

    // The server must see the exact chain the client presented, leaf first —
    // that's what an application maps to a peer identity.
    let seen = peer_certs
        .await?
        .context("server saw no client certificate")?;
    assert_eq!(seen, vec![client_leaf]);

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_untrusted_ca_reject() -> Result<()> {
    init_tracing();

    // The client's cert is signed by a CA the server doesn't trust.
    let ca = Ca::new("web-transport test CA")?;
    let other = Ca::new("web-transport other CA")?;
    let (server_chain, server_key) = ca.server_cert()?;
    let (client_chain, client_key) = other.client_cert("client.example")?;

    let (addr, server, _peer_certs) = spawn_server(
        server_chain,
        server_key,
        ClientAuth::Required(vec![ca.root.clone()]),
    )
    .await?;

    let url = url_for(addr)?;
    let client_bind = loopback_for(addr);

    let result = tokio::time::timeout(Duration::from_secs(5), async move {
        ClientBuilder::default()
            .with_bind(client_bind)?
            .with_root_certificates(vec![ca.root])
            .with_single_cert(client_chain, client_key)
            .connect(url)
            .await?
            .established()
            .await
    })
    .await
    .context("handshake neither succeeded nor failed within the timeout")?;

    assert!(
        result.is_err(),
        "handshake must fail when the client cert chains to an untrusted root"
    );

    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_required_but_missing_reject() -> Result<()> {
    init_tracing();

    let ca = Ca::new("web-transport test CA")?;
    let (server_chain, server_key) = ca.server_cert()?;

    let (addr, server, _peer_certs) = spawn_server(
        server_chain,
        server_key,
        ClientAuth::Required(vec![ca.root.clone()]),
    )
    .await?;

    let url = url_for(addr)?;
    let client_bind = loopback_for(addr);

    // No client certificate at all.
    let result = tokio::time::timeout(Duration::from_secs(5), async move {
        ClientBuilder::default()
            .with_bind(client_bind)?
            .with_root_certificates(vec![ca.root])
            .connect(url)
            .await?
            .established()
            .await
    })
    .await
    .context("handshake neither succeeded nor failed within the timeout")?;

    assert!(
        result.is_err(),
        "handshake must fail when a required client cert is missing"
    );

    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_optional_and_missing_accept() -> Result<()> {
    init_tracing();

    let ca = Ca::new("web-transport test CA")?;
    let (server_chain, server_key) = ca.server_cert()?;

    let (addr, server, peer_certs) = spawn_server(
        server_chain,
        server_key,
        ClientAuth::Optional(vec![ca.root.clone()]),
    )
    .await?;

    let session = ClientBuilder::default()
        .with_bind(loopback_for(addr))?
        .with_root_certificates(vec![ca.root])
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("optional client auth must accept a client without a certificate")?;

    // An unauthenticated peer must be distinguishable from an authenticated one.
    assert!(
        peer_certs.await?.is_none(),
        "server must report no client certificate"
    );

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_optional_and_present_accept() -> Result<()> {
    init_tracing();

    let ca = Ca::new("web-transport test CA")?;
    let (server_chain, server_key) = ca.server_cert()?;
    let (client_chain, client_key) = ca.client_cert("client.example")?;
    let client_leaf = client_chain[0].clone();

    let (addr, server, peer_certs) = spawn_server(
        server_chain,
        server_key,
        ClientAuth::Optional(vec![ca.root.clone()]),
    )
    .await?;

    let session = ClientBuilder::default()
        .with_bind(loopback_for(addr))?
        .with_root_certificates(vec![ca.root])
        .with_single_cert(client_chain, client_key)
        .connect(url_for(addr)?)
        .await?
        .established()
        .await
        .context("handshake should succeed with a client cert from the trusted CA")?;

    // Optional auth still has to surface the identity of a client that presented one.
    let seen = peer_certs
        .await?
        .context("server saw no client certificate")?;
    assert_eq!(seen, vec![client_leaf]);

    session.close(0, "bye");
    session.closed().await;
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_optional_but_untrusted_reject() -> Result<()> {
    init_tracing();

    // Optional means "may omit", not "may present anything".
    let ca = Ca::new("web-transport test CA")?;
    let other = Ca::new("web-transport other CA")?;
    let (server_chain, server_key) = ca.server_cert()?;
    let (client_chain, client_key) = other.client_cert("client.example")?;

    let (addr, server, _peer_certs) = spawn_server(
        server_chain,
        server_key,
        ClientAuth::Optional(vec![ca.root.clone()]),
    )
    .await?;

    let url = url_for(addr)?;
    let client_bind = loopback_for(addr);

    let result = tokio::time::timeout(Duration::from_secs(5), async move {
        ClientBuilder::default()
            .with_bind(client_bind)?
            .with_root_certificates(vec![ca.root])
            .with_single_cert(client_chain, client_key)
            .connect(url)
            .await?
            .established()
            .await
    })
    .await
    .context("handshake neither succeeded nor failed within the timeout")?;

    assert!(
        result.is_err(),
        "an untrusted client cert must fail even when client auth is optional"
    );

    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cert_required_survives_verify_peer_setting() -> Result<()> {
    init_tracing();

    let ca = Ca::new("web-transport test CA")?;
    let (server_chain, server_key) = ca.server_cert()?;

    // quiche applies `verify_peer` after the TLS hook. If the server let it
    // through it would replace the hook's verification mode, quietly turning a
    // required client certificate into an optional one.
    let mut settings = Settings::default();
    settings.verify_peer = true;

    let bind = ("localhost", 0)
        .to_socket_addrs()?
        .next()
        .context("localhost did not resolve")?;

    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_settings(settings)
        .with_client_auth(ClientAuth::Required(vec![ca.root.clone()]))
        .with_single_cert(server_chain, server_key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let handle = tokio::spawn(async move { server.accept().await.is_some() });

    let url = url_for(addr)?;
    let client_bind = loopback_for(addr);

    // No client certificate.
    let result = tokio::time::timeout(Duration::from_secs(5), async move {
        ClientBuilder::default()
            .with_bind(client_bind)?
            .with_root_certificates(vec![ca.root])
            .connect(url)
            .await?
            .established()
            .await
    })
    .await
    .context("handshake neither succeeded nor failed within the timeout")?;

    assert!(
        result.is_err(),
        "a required client cert must stay required regardless of Settings::verify_peer"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn client_auth_without_roots_is_rejected() -> Result<()> {
    init_tracing();

    let ca = Ca::new("web-transport test CA")?;
    let (server_chain, server_key) = ca.server_cert()?;

    // An empty root store would reject every client; that's a config error, not
    // a policy, so it must surface at build time rather than at handshake time.
    let result = ServerBuilder::default()
        .with_bind((Ipv6Addr::LOCALHOST, 0))?
        .with_client_auth(ClientAuth::Required(Vec::new()))
        .with_single_cert(server_chain, server_key);

    assert!(
        result.is_err(),
        "client auth with no roots must fail to build"
    );

    Ok(())
}
