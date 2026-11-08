//! Drive a real quiche session through the poll surface.
//!
//! The round trips call no `async` method on the transport — every operation goes
//! through `web_transport_trait::poll`, stepped with `poll_fn`. Unlike quinn, quiche
//! reaches this with no retained futures at all: `ez` exposes a `poll_*` for every
//! step, so opening a stream is a plain state machine over them. The waker tests also
//! cover the inherent `async` methods, which must not share the poll bridge's
//! retained waiter.

use std::{
    future::{poll_fn, Future},
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
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
    assert_eq!(
        web_transport_trait::SendStream::stats(&mut send)
            .await?
            .bytes_written,
        Some(5)
    );

    let mut recv = poll_fn(|cx| server.poll_accept_uni(cx)).await?;

    let mut dst = [0u8; 16];
    let size = poll_fn(|cx| recv.poll_read(cx, &mut dst))
        .await?
        .context("stream ended early")?;
    assert_eq!(&dst[..size], b"hello");
    assert_eq!(
        web_transport_trait::RecvStream::stats(&mut recv)
            .await?
            .bytes_read,
        Some(5)
    );

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

/// A waker whose `Arc` count the test watches: anything still holding a clone is
/// retaining this allocation. Not `Waker::noop`, which is a static and so cannot be
/// counted.
#[derive(Default)]
struct FlagWaker {
    woken: AtomicBool,
}

impl Wake for FlagWaker {
    fn wake(self: Arc<Self>) {
        self.woken.store(true, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::SeqCst);
    }
}

/// A poll that finishes must not keep the poller's waker.
///
/// The bridge from `Context` to a `kio::Waiter` has to retain the waiter while a poll is
/// pending, or the registration it made dies with the poll. Retaining it past a `Ready`
/// is a different thing entirely: the stream is done with that caller, but goes on
/// holding its waker — and so the polling task's allocation — until something polls it
/// again. A stream whose last read completed and then sat idle would hold one for the
/// rest of the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_poll_releases_the_poller() -> Result<()> {
    let (mut client, mut server) = pair().await?;

    // An opening byte, so the stream reaches the peer and can be accepted.
    let mut send = poll_fn(|cx| client.poll_open_uni(cx)).await?;
    poll_fn(|cx| send.poll_write(cx, b"hi")).await?;

    let mut recv = poll_fn(|cx| server.poll_accept_uni(cx)).await?;
    let mut drained = [0u8; 2];
    assert_eq!(
        poll_fn(|cx| recv.poll_read(cx, &mut drained))
            .await?
            .context("stream ended early")?,
        2
    );

    // Poll until the read is ready, tracking only the waker that sees it through.
    let mut dst = [0u8; 16];
    let flag = Arc::new(FlagWaker::default());
    let waker = Waker::from(flag.clone());

    // Park first, so the waiter this poll retains is the one the `Ready` below has to
    // let go of. Nothing more has been sent and the stream is open, so this cannot be
    // ready yet.
    assert!(recv
        .poll_read(&mut Context::from_waker(&waker), &mut dst)
        .is_pending());

    poll_fn(|cx| send.poll_write(cx, b"done")).await?;

    loop {
        match recv.poll_read(&mut Context::from_waker(&waker), &mut dst) {
            Poll::Ready(res) => {
                assert_eq!(res?.context("stream ended early")?, 4);
                break;
            }
            // Not arrived yet: yield and retry, so the tracked waker is the one that
            // completes the read rather than one left parked.
            Poll::Pending => tokio::task::yield_now().await,
        }
    }

    // The test's own `Waker` counts too, so it goes first.
    drop(waker);
    assert_eq!(
        Arc::strong_count(&flag),
        1,
        "the finished read is still holding the poller's waker"
    );

    Ok(())
}

#[path = "../../web-transport-trait/tests/support/stream_stats.rs"]
mod stream_stats;

#[tokio::test]
async fn stream_payload_statistics() -> Result<()> {
    let (client, server) = pair().await?;
    stream_stats::payload_stats(&client, &server).await;
    Ok(())
}

#[path = "../../web-transport-trait/tests/support/tokio_stream_stats.rs"]
mod tokio_stream_stats;

#[tokio::test]
async fn tokio_stream_payload_statistics() -> Result<()> {
    let (client, server) = pair().await?;
    tokio_stream_stats::payload_stats(&client, &server).await;
    Ok(())
}

#[tokio::test]
async fn empty_inherent_read_buffer_preserves_completion_and_statistics() -> Result<()> {
    let (client, server) = pair().await?;
    let mut send = client.open_uni().await?;
    send.write_all(b"payload").await?;
    send.finish()?;
    let mut recv = server.accept_uni().await?;
    let mut empty: &mut [u8] = &mut [];
    assert_eq!(recv.read_buf(&mut empty).await?, None);
    assert_eq!(recv.stats().await?.bytes_read, Some(0));
    assert_eq!(recv.read_all(7).await?.as_ref(), b"payload");
    assert_eq!(recv.stats().await?.bytes_read, Some(7));
    Ok(())
}

/// Wait until nothing but the test holds `flag`, failing if something keeps a clone.
async fn released(flag: &Arc<FlagWaker>, what: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(flag) > 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| format!("the stream still holds the waker of {what}"))
}

/// Dropping a pending `async` read releases its waiter with the future.
///
/// The `async` methods own their waiter, unlike the poll bridge, which has to retain one
/// between polls. A cancelled read on a stream the caller keeps must not leave its
/// registration and task waker behind once the stream's own wakeup slot is drained.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_async_read_releases_the_poller() -> Result<()> {
    let (client, server) = pair().await?;

    // An opening byte, so the stream reaches the peer and can be accepted.
    let mut send = client.open_uni().await?;
    send.write_all(b"h").await?;
    let mut recv = server.accept_uni().await?;
    assert_eq!(recv.read(&mut [0u8; 1]).await?, Some(1));

    let flag = Arc::new(FlagWaker::default());
    let waker = Waker::from(flag.clone());
    let mut dst = [0u8; 16];
    {
        let mut read = std::pin::pin!(recv.read(&mut dst));
        assert!(read
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
    }
    drop(waker);

    // An arrival wakes and empties the stream's wakeup slot; after that, only a
    // retained waiter could still hold the cancelled caller.
    send.write_all(b"more").await?;
    released(&flag, "the cancelled read").await?;

    assert_eq!(recv.read_all(4).await?.as_ref(), b"more");
    assert_eq!(recv.stats().await?.bytes_read, Some(5));

    Ok(())
}

/// Dropping a pending `async` write releases its waiter with the future.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_async_write_releases_the_poller() -> Result<()> {
    let (client, server) = pair().await?;

    let mut send = client.open_uni().await?;
    let flag = Arc::new(FlagWaker::default());
    let waker = Waker::from(flag.clone());

    // Fill the peer's flow control window until a write parks, then cancel that write.
    let chunk = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let mut write = std::pin::pin!(send.write(&chunk));
        match write.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Ready(size) => written += size? as u64,
            Poll::Pending => break,
        }
    }
    drop(waker);

    // Reading on the peer grants more credit, which wakes and empties the stream's
    // wakeup slot; after that, only a retained waiter could still hold the caller.
    let reader = tokio::spawn(async move {
        let mut recv = server.accept_uni().await?;
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        while let Some(size) = recv.read(&mut buf).await? {
            total += size as u64;
        }
        anyhow::Ok(total)
    });
    released(&flag, "the cancelled write").await?;

    send.write_all(b"done").await?;
    send.finish()?;
    assert_eq!(reader.await??, written + 4);
    assert_eq!(send.stats().await?.bytes_written, Some(written + 4));

    Ok(())
}
