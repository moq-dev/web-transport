//! End-to-end datagram round-trip: client sends N datagrams, server echoes,
//! client reads them back. Exercises the full plumbing path:
//! `ez::Driver` event loop <-> `ez::Connection` flume channels <->
//! `web_transport_quiche::Connection` header framing <-> WT `Session` trait.

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    // SANs cover both hostname and loopback literal — rustls refuses to verify
    // "localhost" against a cert with only a 127.0.0.1 SAN and vice versa, even
    // with verify_peer off on the client we still want a real usable cert.
    let CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&key_pair);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

fn dgram_settings() -> Settings {
    // tokio-quiche defaults already enable datagrams with a 65536-entry queue,
    // but set them explicitly so the test doesn't silently regress if the
    // upstream default ever flips.
    let mut s = Settings::default();
    s.enable_dgram = true;
    s.dgram_recv_max_queue_len = 1024;
    s.dgram_send_max_queue_len = 1024;
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn datagram_round_trip() -> Result<()> {
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
        .with_settings(dgram_settings())
        .with_single_cert(chain, key)?;

    let server_addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let server_task = tokio::spawn(async move {
        let request = server
            .accept()
            .await
            .context("server closed before accepting")?;
        let session = request.ok().await.context("server accept session")?;

        // Echo exactly three datagrams, then return.
        for _ in 0..3 {
            let data = session.read_datagram().await.context("server recv")?;
            session.send_datagram(data).context("server send")?;
        }

        // Give the driver a moment to actually flush the last datagram before
        // we drop the session — otherwise the close races the send.
        tokio::time::sleep(Duration::from_millis(100)).await;
        anyhow::Ok(())
    });

    let mut client_settings = dgram_settings();
    client_settings.verify_peer = false;

    let url = Url::parse(&format!("https://localhost:{}/", server_addr.port()))?;
    let client = ClientBuilder::default()
        .with_settings(client_settings)
        .with_bind((Ipv4Addr::LOCALHOST, 0))?;

    let session = client
        .connect(url)
        .await?
        .established()
        .await
        .context("client handshake")?;

    let payloads: [&[u8]; 3] = [b"hello", b"quic-datagrams", b"round-trip"];

    for p in payloads {
        session.send_datagram(Bytes::copy_from_slice(p))?;
    }

    // Collect echoes. Datagrams are unreliable in general, but on loopback with
    // no congestion every one should make it, so assert ordered equality.
    for expected in payloads {
        let got = tokio::time::timeout(Duration::from_secs(5), session.read_datagram())
            .await
            .context("client recv timed out")??;
        if got.as_ref() != expected {
            bail!(
                "datagram mismatch: got {:?}, want {:?}",
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(expected),
            );
        }
    }

    session.close(0, "bye");
    session.closed().await;

    server_task
        .await
        .context("server task panicked")?
        .context("server task errored")?;

    Ok(())
}
