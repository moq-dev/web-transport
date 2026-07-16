//! Raw QUIC sessions: a QUIC connection using a non-HTTP/3 ALPN, wrapped in a
//! [`Session`] so callers can treat WebTransport and raw QUIC uniformly.
//!
//! There is no CONNECT request on this path, so there is no request URL. The
//! negotiated ALPN is carried by the response instead.

// A crypto provider is needed to establish the connection, matching the gate on
// the crate's own builders.
#![cfg(any(feature = "aws-lc-rs", feature = "ring"))]

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
// The trait provides `protocol()`; imported anonymously to avoid clashing with
// the concrete `Session`.
use web_transport_quinn::generic::Session as _;
use web_transport_quinn::{proto::ConnectResponse, Session};

/// A raw QUIC ALPN, i.e. anything other than the `h3` used by WebTransport.
const RAW_ALPN: &str = "moq-00";

fn self_signed() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).context("rcgen")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
        &signing_key,
    )));

    Ok((cert_der, key_der))
}

/// The provider is passed explicitly rather than relying on the process-level
/// default, which rustls cannot determine when both backend features are enabled
/// (as `--all-features` does).
fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    #[cfg(feature = "aws-lc-rs")]
    {
        Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    }
    #[cfg(all(feature = "ring", not(feature = "aws-lc-rs")))]
    {
        Arc::new(rustls::crypto::ring::default_provider())
    }
}

/// Bind a QUIC server and connect a client to it, both offering only `RAW_ALPN`.
async fn connect_raw() -> Result<(quinn::Connection, quinn::Connection)> {
    let (cert, key) = self_signed()?;
    let provider = crypto_provider();

    let mut server_crypto = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)?;
    server_crypto.alpn_protocols = vec![RAW_ALPN.as_bytes().to_vec()];

    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));

    let bind: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let server = quinn::Endpoint::server(server_config, bind)?;
    let server_addr = server.local_addr()?;

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert)?;

    let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![RAW_ALPN.as_bytes().to_vec()];

    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
    ));

    let mut client = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    client.set_default_client_config(client_config);

    let accept = tokio::spawn(async move {
        let conn = server.accept().await.context("no connection")?.await?;
        anyhow::Ok(conn)
    });

    let client_conn = client.connect(server_addr, "localhost")?.await?;
    let server_conn = accept.await??;

    Ok((client_conn, server_conn))
}

/// The ALPN the peers negotiated, as an accept path would read it.
fn negotiated_alpn(conn: &quinn::Connection) -> Result<String> {
    let data = conn
        .handshake_data()
        .context("no handshake data")?
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .ok()
        .context("unexpected handshake data")?;

    Ok(String::from_utf8(data.protocol.context("no ALPN")?)?)
}

/// A raw session has no CONNECT request, so it has no URL. Before this was an
/// `Option`, callers had to synthesize a fake request to construct one.
#[tokio::test]
async fn raw_session_has_no_request() -> Result<()> {
    let (client_conn, _server_conn) = connect_raw().await?;

    let session = Session::raw(client_conn, ConnectResponse::OK);
    assert!(session.request().is_none());

    Ok(())
}

/// The response is what carries the negotiated ALPN on the raw path.
#[tokio::test]
async fn raw_session_reports_alpn_as_protocol() -> Result<()> {
    let (client_conn, _server_conn) = connect_raw().await?;

    let alpn = negotiated_alpn(&client_conn)?;
    assert_eq!(alpn, RAW_ALPN);

    let session = Session::raw(client_conn, ConnectResponse::OK.with_protocol(&alpn));
    assert_eq!(session.protocol(), Some(RAW_ALPN));

    Ok(())
}

/// A raw session must not write the WebTransport stream header, since the peer
/// is reading plain QUIC.
#[tokio::test]
async fn raw_session_streams_omit_webtransport_header() -> Result<()> {
    let (client_conn, server_conn) = connect_raw().await?;

    let client = Session::raw(client_conn, ConnectResponse::OK);
    let server = Session::raw(server_conn, ConnectResponse::OK);

    let mut send = client.open_uni().await?;
    send.write_all(b"hello").await?;
    send.finish()?;

    let mut recv = server.accept_uni().await?;
    let data = recv.read_to_end(1024).await?;
    assert_eq!(&data, b"hello");

    Ok(())
}
