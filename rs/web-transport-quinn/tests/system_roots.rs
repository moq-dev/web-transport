//! `with_system_roots()` verifies through the OS platform verifier, which must reject a
//! server whose certificate no system root vouches for.

use std::net::Ipv4Addr;

use anyhow::Result;
use rcgen::{CertifiedKey, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use web_transport_proto::ConnectRequest;
use web_transport_quinn::{quinn, ClientBuilder, ClientError, ServerBuilder};

#[tokio::test]
async fn system_roots_reject_self_signed_server() -> Result<()> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
        &signing_key,
    )));

    let mut server = ServerBuilder::new()
        .with_addr((Ipv4Addr::LOCALHOST, 0).into())
        .with_certificate(vec![cert.der().clone()], key)?;
    let addr = server.local_addr()?;
    let server_task = tokio::spawn(async move { while server.accept().await.is_some() {} });

    let url: url::Url = format!("https://localhost:{}/", addr.port()).parse()?;
    let client = ClientBuilder::new().with_system_roots()?;
    let err = client
        .connect(ConnectRequest::new(url))
        .await
        .expect_err("a self-signed certificate must not verify");

    assert!(
        matches!(&err, ClientError::Connection(quinn::ConnectionError::TransportError(e)) if e.reason.contains("UnknownIssuer")),
        "{err:?}"
    );

    server_task.abort();
    Ok(())
}
