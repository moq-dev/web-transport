//! Regression: resetting or dropping an individual stream must never tear down
//! the whole connection.
//!
//! On a short-lived request/response "control stream", the requester drops the
//! stream after the peer has already closed its send half (here via STOP_SENDING).
//! The resulting RESET_STREAM then lands on an already-closed send, where quiche's
//! `stream_shutdown` returns `Done` — which used to be propagated as a fatal
//! connection error, killing every other stream on the session. This is exactly
//! the lite-05 TRACK stream pattern that broke moq-native's quiche backend tests.

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&key_pair);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

// Current-thread runtime on purpose: it makes the FIN-vs-`stream_shutdown`
// interleaving deterministic, which is what surfaces the bug. A multi-threaded
// runtime schedules around the race and hides the regression.
#[tokio::test(flavor = "current_thread")]
async fn reset_stream_keeps_connection_alive() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();

    let (chain, key) = make_self_signed()?;

    let bind: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_single_cert(chain, key)?;

    let server_addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    // Server: for every accepted control stream, reply + FIN immediately and drop
    // the request side without reading it. Dropping the receiver emits STOP_SENDING,
    // which closes the client's send half — so when the client later drops the
    // stream, its RESET_STREAM lands on an already-closed send (the exact case that
    // made `stream_shutdown` return `Done`). A peer reset must never propagate to
    // the session.
    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;

        loop {
            let (mut send, recv) = match session.accept_bi().await {
                Ok(pair) => pair,
                Err(_) => break, // session closed by the client
            };

            tokio::spawn(async move {
                let _ = send.write_all(b"pong").await;
                let _ = send.finish();
                drop(recv); // STOP_SENDING to the client's send half
                let _ = send.closed().await;
            });
        }

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

    // Open a control stream, read the reply, then drop it. Crucially the previous
    // stream is dropped only *after* the next one is opened, mirroring lite-05's
    // "drop the TRACK stream, open the SUBSCRIBE stream" choreography that
    // interleaves a RESET_STREAM with new stream activity in one driver poll.
    let mut prev = None;
    for i in 0..20 {
        let (mut send, mut recv) = session.open_bi().await.context("open control stream")?;
        // The send half may already be stopped by the server; ignore that error.
        let _ = send.write_all(b"info").await;

        let mut buf = [0u8; 4];
        let mut got = 0;
        while got < buf.len() {
            match recv.read(&mut buf[got..]).await.context("read reply")? {
                Some(n) if n > 0 => got += n,
                _ => break,
            }
        }
        assert_eq!(&buf[..got], b"pong", "control stream {i} reply");

        // Hold this stream, drop the previous one now (RESET_STREAM on a send the
        // server already stopped). Drop happens while this iteration's stream is live.
        prev = Some((send, recv));
    }
    drop(prev);

    // The connection must still be fully usable after all those resets.
    let (mut send, mut recv) = session.open_bi().await.context("open liveness stream")?;
    send.write_all(b"info")
        .await
        .context("write liveness request")?;
    send.finish().context("finish liveness request")?;
    let reply = recv.read_all(1024).await.context("read liveness reply")?;
    assert_eq!(
        reply.as_ref(),
        b"pong",
        "connection was torn down by an individual stream reset"
    );

    session.close(0, "bye");
    session.closed().await;

    server_task
        .await
        .context("server task panicked")?
        .context("server task errored")?;

    Ok(())
}
