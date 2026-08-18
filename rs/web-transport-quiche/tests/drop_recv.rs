//! Regression: dropping a receive stream early must not close the session, and it
//! must leave the sender's half in a state the application can observe.
//!
//! moq-lite-05 opens a unidirectional TRACK stream, writes a header, and holds the
//! stream open. The subscriber reads the header and drops the receiver because it
//! needs nothing else, which sends STOP_SENDING. quiche resets the sender's half in
//! response and collects it entirely once that RESET_STREAM is acked, so anything
//! the sender does next lands on a stream quiche no longer knows about and comes
//! back as `quiche::Error::Done`. That is per-stream teardown, but it used to be
//! promoted into a fatal connection error: application close code 500, reason
//! `Done`.

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{
    ClientBuilder, Connection, RecvStream, SendStream, Server, ServerBuilder, SessionError,
    Settings, StreamError,
};

const HEADER: &[u8] = b"TRACK_INFO";

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

fn start_server() -> Result<(Server, SocketAddr)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let chain = vec![CertificateDer::from(cert.der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
        &signing_key,
    )));

    let bind: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let server = ServerBuilder::default()
        .with_bind(bind)?
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

    let session = client
        .connect(url)
        .await?
        .established()
        .await
        .context("client handshake")?;

    Ok(session)
}

/// Read exactly `buf.len()` bytes, so the two sides stay in lockstep.
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

// Current-thread runtime on purpose, matching the sibling reset test: it keeps the
// teardown interleaving deterministic.
#[tokio::test(flavor = "current_thread")]
async fn drop_recv_keeps_connection_alive() -> Result<()> {
    const STREAMS: usize = 16;

    init_tracing();
    let (mut server, addr) = start_server()?;

    // Server: open a batch of unidirectional streams, write a header on each, and
    // hold the send halves open. A bidirectional stream sequences the rest: the
    // second "ping" only arrives after the client has acked the RESET_STREAM for
    // every dropped receiver, which is what makes the streams collected — and the
    // resulting `Done` — deterministic.
    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;

        let mut sends = Vec::new();
        for _ in 0..STREAMS {
            let mut send = session.open_uni().await.context("server open_uni")?;
            write_msg(&mut send, HEADER)
                .await
                .context("server header")?;
            sends.push(send);
        }

        let (mut sync_send, mut sync_recv) = session.accept_bi().await.context("server sync")?;
        let mut ping = [0u8; 4];
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 1")?;
        write_msg(&mut sync_send, b"pong").await?;
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 2")?;

        for mut send in sends {
            // The peer stopped these streams, so finishing them is a no-op — but it
            // must never take the session down with it.
            let _ = send.finish();
        }

        // Only observable if the session survived every one of those finishes.
        write_msg(&mut sync_send, b"done").await?;

        anyhow::Ok(session.closed().await)
    });

    let session = connect_client(addr).await?;

    for i in 0..STREAMS {
        let mut recv = session.accept_uni().await.context("client accept_uni")?;

        let mut header = [0u8; HEADER.len()];
        read_exact(&mut recv, &mut header)
            .await
            .with_context(|| format!("stream {i} header"))?;
        assert_eq!(&header, HEADER, "stream {i} header");

        // Everything needed has been read; drop without reading to FIN.
        drop(recv);
    }

    let (mut sync_send, mut sync_recv) = session.open_bi().await.context("client sync")?;
    write_msg(&mut sync_send, b"ping").await?;

    // The reply shares a flight with the server's RESET_STREAMs, so the second ping
    // carries the ack that collects them.
    let mut pong = [0u8; 4];
    read_exact(&mut sync_recv, &mut pong)
        .await
        .context("pong")?;
    write_msg(&mut sync_send, b"ping").await?;

    let mut done = [0u8; 4];
    read_exact(&mut sync_recv, &mut done)
        .await
        .context("session was torn down by an early receive-stream drop")?;
    assert_eq!(&done, b"done");

    session.close(0, "bye");
    session.closed().await;

    // The server must have seen the client's close, not a locally generated one.
    let err = server_task
        .await
        .context("server task panicked")?
        .context("server task errored")?;
    assert!(
        matches!(err, SessionError::Remote(0, _)),
        "unexpected session error: {err}"
    );

    Ok(())
}

/// Once quiche has collected the stream, the sender's cached capacity is stale. A
/// write must fail rather than queue bytes onto state the driver has already
/// retired, which nothing will ever flush or wake again.
#[tokio::test(flavor = "current_thread")]
async fn write_after_drop_recv_ends_the_stream() -> Result<()> {
    init_tracing();
    let (mut server, addr) = start_server()?;

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;

        let mut send = session.open_uni().await.context("server open_uni")?;
        write_msg(&mut send, HEADER)
            .await
            .context("server header")?;

        let (mut sync_send, mut sync_recv) = session.accept_bi().await.context("server sync")?;
        let mut ping = [0u8; 4];
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 1")?;
        write_msg(&mut sync_send, b"pong").await?;
        read_exact(&mut sync_recv, &mut ping)
            .await
            .context("ping 2")?;

        // The stream is stopped and collected by now. The first writes may still be
        // accepted against the stale capacity, but the stream has to fail them before
        // that capacity runs out, or the writer parks with nothing left to wake it.
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

        // Hold the session (and with it the sync stream) until the client is done,
        // so "done" is flushed rather than reset by the teardown.
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
        .context("server never finished writing to the collected stream")?;
    assert_eq!(&done, b"done");

    session.close(0, "bye");
    session.closed().await;

    let err = server_task
        .await
        .context("server task panicked")?
        .context("server task errored")?;
    assert!(
        matches!(err, StreamError::Closed),
        "unexpected stream error: {err}"
    );

    Ok(())
}
