//! Writing to a send stream the peer stopped reports the peer's code without stalling
//! its session.
//!
//! Once quiche reports the peer's STOP_SENDING, `SendState::flush` marks the stream
//! closed and the driver drops its entry. The application usually finds out one
//! write later. A separate bidirectional stream must still make progress after that
//! write fails.

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{
    ClientBuilder, Connection, RecvStream, SendStream, Server, ServerBuilder, Settings, StreamError,
};

const HEADER: &[u8] = b"TRACK_INFO";

fn start_server() -> Result<(Server, SocketAddr)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let chain = vec![CertificateDer::from(cert.der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
        &signing_key,
    )));

    let server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    Ok((server, addr))
}

async fn connect_client(addr: SocketAddr) -> Result<Connection> {
    let mut settings = Settings::default();
    settings.verify_peer = false;

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let client = ClientBuilder::default()
        .with_settings(settings)
        .with_bind((Ipv4Addr::LOCALHOST, 0))?;

    client
        .connect(url)
        .await?
        .established()
        .await
        .context("client handshake")
}

async fn read_exact(recv: &mut RecvStream, buf: &mut [u8]) -> Result<()> {
    let mut got = 0;
    while got < buf.len() {
        match recv.read(&mut buf[got..]).await.context("read")? {
            Some(n) if n > 0 => got += n,
            _ => anyhow::bail!("stream ended after {got} of {} bytes", buf.len()),
        }
    }
    Ok(())
}

async fn write_msg(send: &mut SendStream, msg: &[u8]) -> Result<()> {
    send.write_all(msg).await.context("write")?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn writing_to_a_stopped_stream_returns_the_stop_code() -> Result<()> {
    let (mut server, addr) = start_server()?;

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;

        let mut send = session.open_uni().await.context("server open_uni")?;
        write_msg(&mut send, HEADER).await.context("header")?;

        // The second ping only lands after the client's STOP_SENDING has arrived and
        // the server's RESET_STREAM has been acked.
        let (mut sync_send, mut sync_recv) = session.accept_bi().await.context("server sync")?;
        let mut ping = [0u8; 4];
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 1")?;
        write_msg(&mut sync_send, b"pong").await?;
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 2")?;

        // Keep writing until the stopped stream reports itself over.
        let chunk = vec![0u8; 64 * 1024];
        let err: StreamError = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Err(e) = send.write_all(&chunk).await {
                    return e;
                }
            }
        })
        .await
        .context("writing to a stopped stream never returned")?;

        write_msg(&mut sync_send, b"done").await?;
        session.closed().await;

        anyhow::Ok(err)
    });

    let session = connect_client(addr).await?;

    let mut recv = session.accept_uni().await.context("client accept_uni")?;
    let mut header = [0u8; HEADER.len()];
    read_exact(&mut recv, &mut header).await.context("header")?;
    assert_eq!(&header, HEADER);
    drop(recv);

    let (mut sync_send, mut sync_recv) = session.open_bi().await.context("client sync")?;
    write_msg(&mut sync_send, b"ping").await?;
    let mut pong = [0u8; 4];
    read_exact(&mut sync_recv, &mut pong)
        .await
        .context("pong")?;
    write_msg(&mut sync_send, b"ping").await?;

    let mut done = [0u8; 4];
    read_exact(&mut sync_recv, &mut done)
        .await
        .context("done")?;
    assert_eq!(&done, b"done");

    session.close(0, "bye");
    session.closed().await;

    let err = server_task
        .await
        .context("server task panicked")?
        .context("server task errored")?;

    // A dropped `RecvStream` stops the stream with "DECV".
    assert!(
        matches!(err, StreamError::Stop(0x44454356)),
        "expected the peer's stop code to end the write, got: {err}"
    );

    Ok(())
}
