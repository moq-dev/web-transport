//! A caller that gives up must not leave its waker inside `ez`.
//!
//! Every `ez` poll that parks registers with the connection: an accept joins the accept
//! queue's list, and *any* blocked operation — read, write, accept, handshake — also
//! registers with the connection-closed list so a teardown wakes it. Those were plain
//! `Vec<Waker>`s cleared only when the thing they waited for happened, and the
//! connection-closed one did not even deduplicate, so it grew by a waker clone on every
//! pending poll and held them all until the connection went away.
//!
//! Nothing here lets the operations complete: the point is what the connection retains
//! while they are pending, and an arrival or a close drains the lists.

use std::{
    future::Future,
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
use tokio::{
    io::{AsyncRead, ReadBuf},
    time::timeout,
};
use web_transport_quiche::ez;

/// How many callers give up, one after another. Well past any plausible bound on
/// concurrent callers, so a list that keeps every registration is obvious.
const ABANDONED: usize = 256;

/// A waker whose `Arc` count the test watches: anything still holding a clone is
/// retaining this allocation.
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

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

/// A connected client and server, raw QUIC with no WebTransport on top.
async fn pair() -> Result<(ez::Connection, ez::Connection)> {
    let (chain, key) = make_self_signed()?;

    let mut server = ez::ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain.clone(), key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.context("no connection")?;
        let conn = incoming.accept().await?;
        anyhow::Ok(conn)
    });

    let hashes = chain
        .iter()
        .map(|cert| {
            use sha2::Digest;
            let digest = sha2::Sha256::digest(cert.as_ref());
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&digest);
            hash
        })
        .collect();

    let client = ez::ClientBuilder::new()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .with_server_certificate_hashes(hashes)
        .connect("127.0.0.1", addr.port())
        .await?
        .established()
        .await?;

    let server = timeout(Duration::from_secs(10), server_task)
        .await
        .context("the server never accepted")???;

    Ok((client, server))
}

/// Poll an operation once per iteration with a fresh waker, dropping the future each
/// time, and report how many of those wakers something still holds.
///
/// A macro rather than a function taking a closure: each iteration needs its own mutable
/// borrow of the stream being polled, which an `FnMut` cannot hand out.
///
/// The last poll's waker is excluded. Single-owner state — a stream's blocked reader, the
/// driver's own wakeup — legitimately keeps a plain waker for the most recent poller;
/// what must not happen is the *other* 256 piling up.
macro_rules! abandon {
    ($op:expr) => {{
        let mut flags = Vec::with_capacity(ABANDONED);

        for _ in 0..=ABANDONED {
            let flag = Arc::new(FlagWaker::default());
            let waker = Waker::from(flag.clone());

            let mut fut = std::pin::pin!($op);
            assert!(
                fut.as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending(),
                "the operation should not be able to complete"
            );

            flags.push(flag);
        }

        flags.pop();
        flags
            .iter()
            .filter(|flag| Arc::strong_count(flag) > 1)
            .count()
    }};
}

/// A blocked read is the common case: it registers with the connection-closed list on
/// every pending poll, so this is the list that grew fastest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_reads_release_their_wakers() -> Result<()> {
    let (client, server) = pair().await?;

    let mut send = client.open_uni().await?;
    send.write_all(b"hi").await?;

    let mut recv = server.accept_uni().await?;
    let mut buf = [0u8; 2];
    assert_eq!(recv.read(&mut buf).await?, Some(2));

    // The peer sends nothing more and never finishes the stream, so every read parks.
    let retained = abandon!(recv.read(&mut buf));
    assert_eq!(retained, 0, "{retained} abandoned reads are still parked");

    client.close(0, "bye");
    Ok(())
}

/// The accept queue's own list, on a connection where no stream ever arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_accepts_release_their_wakers() -> Result<()> {
    let (client, server) = pair().await?;

    let retained = abandon!(server.accept_uni());
    assert_eq!(retained, 0, "{retained} abandoned accepts are still parked");

    client.close(0, "bye");
    Ok(())
}

/// Waiting on the connection registers with the same list, and a caller that stops
/// waiting must release its slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_datagram_reads_release_their_wakers() -> Result<()> {
    let (client, server) = pair().await?;

    let retained = abandon!(server.read_datagram());
    assert_eq!(
        retained, 0,
        "{retained} abandoned datagram reads are still parked"
    );

    client.close(0, "bye");
    Ok(())
}

/// The other half of the rule: a poll that *finishes* must not keep the poller's waker
/// either. The bridge retains a waiter so its registrations survive a `Pending`; holding
/// it past a `Ready` would pin the polling task's allocation to the stream until
/// something polled it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_read_releases_the_poller() -> Result<()> {
    let (client, server) = pair().await?;

    let mut send = client.open_uni().await?;
    send.write_all(b"done").await?;

    let mut recv = server.accept_uni().await?;

    // Read through `AsyncRead`, which is the surface that has to bridge a `Context`.
    let flag = Arc::new(FlagWaker::default());
    let waker = Waker::from(flag.clone());
    let mut dst = [0u8; 16];
    let mut buf = ReadBuf::new(&mut dst);

    loop {
        let read =
            std::pin::Pin::new(&mut recv).poll_read(&mut Context::from_waker(&waker), &mut buf);
        match read {
            Poll::Ready(res) => {
                res?;
                break;
            }
            // Not arrived yet: yield and retry, so the tracked waker is the one that
            // completes the read rather than one left parked.
            Poll::Pending => tokio::task::yield_now().await,
        }
    }
    assert_eq!(buf.filled(), b"done");

    // The test's own `Waker` counts too, so it goes first.
    drop(waker);
    assert_eq!(
        Arc::strong_count(&flag),
        1,
        "the finished read is still holding the poller's waker"
    );

    client.close(0, "bye");
    Ok(())
}
