use std::{
    net::{Ipv4Addr, SocketAddr},
    task::Poll,
    time::Duration,
};

use anyhow::{Context, Result};
use futures::poll;
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::AsyncWriteExt;
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

#[tokio::test(flavor = "current_thread")]
async fn flush_and_shutdown_complete_after_returning_pending() -> Result<()> {
    let (chain, key) = make_self_signed()?;

    let bind: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_single_cert(chain, key)?;

    let server_addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;
        let mut recv = session.accept_uni().await.context("accept stream")?;
        let data = recv.read_all(1024).await.context("read stream")?;
        anyhow::ensure!(data.as_ref() == b"hello", "unexpected stream contents");
        anyhow::Ok(())
    });

    let mut client_settings = Settings::default();
    client_settings.verify_peer = false;

    let url = Url::parse(&format!("https://127.0.0.1:{}/", server_addr.port()))?;
    let client = ClientBuilder::default()
        .with_settings(client_settings)
        .with_bind((Ipv4Addr::LOCALHOST, 0))?;

    let session = client
        .connect(url)
        .await?
        .established()
        .await
        .context("client handshake")?;

    let mut send = session.open_uni().await.context("open stream")?;
    send.write_all(b"hello").await.context("write stream")?;

    // The current-thread runtime cannot run the driver between these statements,
    // so the data is still in SendState's queue on the first flush poll.
    let mut flush = Box::pin(send.flush());
    assert!(matches!(poll!(&mut flush), Poll::Pending));
    tokio::time::timeout(Duration::from_secs(5), flush)
        .await
        .context("flush timed out")??;

    // The first shutdown poll initiates FIN and must return Pending. A subsequent
    // poll must continue waiting for FIN instead of calling finish() a second time.
    let mut shutdown = Box::pin(send.shutdown());
    assert!(matches!(poll!(&mut shutdown), Poll::Pending));
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .context("shutdown timed out")??;

    server_task
        .await
        .context("server task panicked")?
        .context("server task errored")?;

    session.close(0, "bye");
    session.closed().await;

    Ok(())
}
