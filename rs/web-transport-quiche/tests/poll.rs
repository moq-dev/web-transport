//! Drive a real quiche session entirely through the poll surface.
//!
//! Nothing here calls an `async` method on the transport — every operation goes
//! through `web_transport_trait::poll`, stepped with `poll_fn`. Unlike quinn, quiche
//! reaches this with no retained futures at all: `ez` exposes a `poll_*` for every
//! step, so opening a stream is a plain state machine over them.

use std::{
    future::poll_fn,
    net::{Ipv4Addr, SocketAddr},
};

use anyhow::{Context as _, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};
use web_transport_trait::poll::{RecvStream, SendStream, Session};

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

fn client() -> ClientBuilder {
    let mut settings = Settings::default();
    settings.verify_peer = false;
    ClientBuilder::default().with_settings(settings)
}

/// A client and server session, already connected.
async fn pair() -> Result<(
    web_transport_quiche::Connection,
    web_transport_quiche::Connection,
)> {
    let (chain, key) = make_self_signed()?;

    let mut server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.context("no connection")?;
        anyhow::Ok(request.ok().await?)
    });

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let client = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    let server = server_task.await??;
    Ok((client, server))
}

/// Open, write, finish, accept and read — all through `poll_*`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_uni_stream_round_trips_through_the_poll_surface() -> Result<()> {
    let (mut client, mut server) = pair().await?;

    let mut send = poll_fn(|cx| client.poll_open_uni(cx)).await?;
    poll_fn(|cx| send.poll_write(cx, b"hello")).await?;
    send.finish()?;

    let mut recv = poll_fn(|cx| server.poll_accept_uni(cx)).await?;

    let mut dst = [0u8; 16];
    let size = poll_fn(|cx| recv.poll_read(cx, &mut dst))
        .await?
        .context("stream ended early")?;
    assert_eq!(&dst[..size], b"hello");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bi_stream_round_trips_through_the_poll_surface() -> Result<()> {
    let (mut client, mut server) = pair().await?;

    let (mut client_send, mut client_recv) = poll_fn(|cx| client.poll_open_bi(cx)).await?;
    poll_fn(|cx| client_send.poll_write(cx, b"ping")).await?;

    let (mut server_send, mut server_recv) = poll_fn(|cx| server.poll_accept_bi(cx)).await?;

    let mut dst = [0u8; 16];
    let size = poll_fn(|cx| server_recv.poll_read(cx, &mut dst))
        .await?
        .context("stream ended early")?;
    assert_eq!(&dst[..size], b"ping");

    poll_fn(|cx| server_send.poll_write(cx, b"pong")).await?;
    let size = poll_fn(|cx| client_recv.poll_read(cx, &mut dst))
        .await?
        .context("stream ended early")?;
    assert_eq!(&dst[..size], b"pong");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn datagrams_round_trip_through_the_poll_surface() -> Result<()> {
    let (mut client, mut server) = pair().await?;

    poll_fn(|cx| client.poll_send_datagram(cx, b"dgram")).await?;

    let received = poll_fn(|cx| server.poll_recv_datagram(cx)).await?;
    assert_eq!(&received[..], b"dgram");

    Ok(())
}
