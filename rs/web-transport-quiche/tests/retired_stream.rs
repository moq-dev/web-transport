//! Regression: writing to a stream the driver has already retired must not wake it.
//!
//! Once the peer's STOP_SENDING has quiche collect the send half, `SendState::flush`
//! marks the stream closed and the driver drops its entry. The application usually
//! finds out one write later, and that failing write used to notify the driver
//! anyway, naming a stream it no longer tracks. The driver logged
//! `wakeup for closed stream` and did nothing else.
//!
//! Harmless on its own, but moq-lite-05 publishers keep writing to a TRACK stream
//! after the subscriber has dropped its receiver, so it fires once per write. That
//! is the warning flood reported in moq-dev/web-transport#378.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tracing_subscriber::fmt::MakeWriter;
use url::Url;
use web_transport_quiche::{
    ClientBuilder, Connection, RecvStream, SendStream, Server, ServerBuilder, Settings, StreamError,
};

const HEADER: &[u8] = b"TRACK_INFO";

/// Collects the subscriber's output so the test can assert on what was logged.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

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

// One test per binary: the capturing subscriber is global, so a second concurrent
// test would mix its output into the assertion below.
#[tokio::test(flavor = "current_thread")]
async fn writing_to_a_retired_stream_does_not_wake_the_driver() -> Result<()> {
    let captured = Captured::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish(),
    )
    .context("set global subscriber")?;

    let (mut server, addr) = start_server()?;

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;

        let mut send = session.open_uni().await.context("server open_uni")?;
        write_msg(&mut send, HEADER).await.context("header")?;

        // The second ping only lands after the client has acked the RESET_STREAM,
        // which is what makes the stream collected rather than merely stopped.
        let (mut sync_send, mut sync_recv) = session.accept_bi().await.context("server sync")?;
        let mut ping = [0u8; 4];
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 1")?;
        write_msg(&mut sync_send, b"pong").await?;
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 2")?;

        // Keep writing until the retired stream reports itself over. This is the
        // path that used to notify the driver about a stream it had dropped.
        let chunk = vec![0u8; 64 * 1024];
        let err: StreamError = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Err(e) = send.write_all(&chunk).await {
                    return e;
                }
            }
        })
        .await
        .context("writing to a collected stream never returned")?;

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

    // Without this the assertion below is vacuous: it proves the write actually
    // reached a retired stream rather than succeeding all the way through.
    assert!(
        matches!(err, StreamError::Closed),
        "expected the retired stream to end the write, got: {err}"
    );

    let logged = captured.text();
    assert!(
        !logged.contains("wakeup for closed stream"),
        "driver was woken for a stream it had already retired:\n{logged}"
    );

    Ok(())
}
