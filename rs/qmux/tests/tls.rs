//! QMux over TLS: ALPN selects the wire-format version and application protocol.

#![cfg(feature = "tls")]

use std::{net::SocketAddr, sync::Arc};

use qmux::{tls, Error, Session, Version};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use tokio::{net::TcpListener, task::JoinHandle};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

struct Cert {
    der: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

fn cert() -> Cert {
    let key = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    Cert {
        der: key.cert.der().clone(),
        key: PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()),
    }
}

/// Accept one TLS connection whose server selects from `alpns`.
async fn serve(cert: &Cert, alpns: &[&str]) -> (SocketAddr, JoinHandle<Result<Session, Error>>) {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der.clone()], cert.key.clone_key().into())
        .unwrap();
    config.alpn_protocols = alpns.iter().map(|a| a.as_bytes().to_vec()).collect();
    let server = tls::Server::new(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        server.accept(sock).await
    });
    (addr, task)
}

/// A client trusting only `cert`.
fn client(cert: &Cert) -> tls::Client {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der.clone()).unwrap();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls::Client::new(Arc::new(config))
}

/// Data crosses in both directions, so both ends framed records the same way.
async fn round_trip(client: &Session, server: &Session) {
    let mut send = client.open_uni().await.unwrap();
    send.write(b"ping").await.unwrap();
    send.finish().unwrap();
    let mut recv = server.accept_uni().await.unwrap();
    assert_eq!(recv.read_all().await.unwrap().as_ref(), b"ping");

    let mut send = server.open_uni().await.unwrap();
    send.write(b"pong").await.unwrap();
    send.finish().unwrap();
    let mut recv = client.accept_uni().await.unwrap();
    assert_eq!(recv.read_all().await.unwrap().as_ref(), b"pong");
}

/// A prefixed ALPN carries both the version and the application protocol.
#[tokio::test]
async fn prefixed_alpn_negotiates_version_and_protocol() {
    let cert = cert();
    let (addr, server) = serve(&cert, &["qmux-02.moq-lite-04"]).await;

    let client = client(&cert)
        .with_protocol("moq-lite-03", &[Version::QMux02])
        .with_protocol("moq-lite-04", &[])
        .require_protocol()
        .connect(addr, "localhost")
        .await
        .unwrap();
    let server = server.await.unwrap().unwrap();

    assert_eq!(client.protocol(), Some("moq-lite-04"));
    assert_eq!(server.protocol(), Some("moq-lite-04"));
    round_trip(&client, &server).await;
}

/// Without `require_protocol`, a server that only knows the bare version ALPN
/// still connects, with no application protocol.
#[tokio::test]
async fn bare_alpn_connects_without_protocol() {
    let cert = cert();
    let (addr, server) = serve(&cert, &["qmux-01"]).await;

    let client = client(&cert)
        .with_protocol("moq-lite-04", &[Version::QMux02])
        .connect(addr, "localhost")
        .await
        .unwrap();
    let server = server.await.unwrap().unwrap();

    assert_eq!(client.protocol(), None);
    assert_eq!(server.protocol(), None);
    round_trip(&client, &server).await;
}

/// `require_protocol` refuses a handshake that negotiated no ALPN instead of
/// falling back to the legacy wire format.
#[tokio::test]
async fn require_protocol_rejects_missing_alpn() {
    let cert = cert();
    let (addr, server) = serve(&cert, &[]).await;

    let err = client(&cert)
        .with_protocol("moq-lite-04", &[])
        .require_protocol()
        .connect(addr, "localhost")
        .await
        .err()
        .expect("connected without a negotiated protocol");
    assert!(
        matches!(&err, Error::InvalidProtocol(alpn) if alpn == "<none>"),
        "{err:?}"
    );
    server.abort();
}

/// The server's certificate is verified against the client's roots.
#[tokio::test]
async fn untrusted_certificate_is_rejected() {
    let (addr, server) = serve(&cert(), &["qmux-02"]).await;

    let err = client(&cert())
        .connect(addr, "localhost")
        .await
        .err()
        .expect("connected to an untrusted server");
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert!(server.await.unwrap().is_err());
}

/// The certificate must match the name the client dials.
#[tokio::test]
async fn mismatched_server_name_is_rejected() {
    let cert = cert();
    let (addr, server) = serve(&cert, &["qmux-02"]).await;

    let err = client(&cert)
        .connect(addr, "example.com")
        .await
        .err()
        .expect("connected under the wrong name");
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert!(server.await.unwrap().is_err());
}

/// A server name that is neither a DNS name nor an IP address fails before dialing TLS.
#[tokio::test]
async fn invalid_server_name_is_rejected() {
    let cert = cert();
    let (addr, server) = serve(&cert, &["qmux-02"]).await;

    let err = client(&cert)
        .connect(addr, "not a name")
        .await
        .err()
        .expect("accepted an invalid server name");
    assert!(matches!(err, Error::InvalidServerName), "{err:?}");
    server.abort();
}
