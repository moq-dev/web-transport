//! Concurrent accepters must all be woken, and must not pile up.
//!
//! Every clone of a `Connection` polls the same `SessionAccept`, and it kept no wakers
//! of its own — each poll registered with the inner accept stream, which stores
//! exactly one. A second accepter registered through a waker that replaced the
//! first's, so the first never woke again even as streams arrived.
//!
//! The list that fixed that had no deregistration path, so an accepter that dropped its
//! future left its waker parked forever. Callers now register weakly and the inner
//! accept streams are polled with the accept state's own waker, which is what keeps both
//! this list and `ez`'s bounded.

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
    time::Duration,
};

use anyhow::{Context as _, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::{io::AsyncReadExt, time::timeout};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

/// A waker that records whether it was ever woken.
#[derive(Default)]
struct FlagWaker {
    woken: AtomicBool,
}

impl FlagWaker {
    fn woken(&self) -> bool {
        self.woken.load(Ordering::SeqCst)
    }
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

fn client() -> ClientBuilder {
    let mut settings = Settings::default();
    settings.verify_peer = false;
    ClientBuilder::default().with_settings(settings)
}

/// Register an accepter, abandon it, and repeat — on a connection where no stream ever
/// arrives, which is the case that used to grow forever. Nothing here delivers a
/// stream: an arrival drains the list, which would hide exactly what is under test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_accepters_release_their_wakers() -> Result<()> {
    /// Well past any plausible bound on concurrent accepters.
    const ABANDONED: usize = 256;

    let (chain, key) = make_self_signed()?;

    let mut server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Vec<Arc<FlagWaker>>>();

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.expect("a connection");
        let session = request.ok().await.expect("an established session");

        // Each iteration is a distinct task's worth of waker, registered by a single
        // poll and abandoned when the future drops at the end of the scope.
        let mut abandoned = Vec::with_capacity(ABANDONED);
        for _ in 0..ABANDONED {
            let flag = Arc::new(FlagWaker::default());
            let waker = Waker::from(flag.clone());

            let mut accept = std::pin::pin!(session.accept_uni());
            assert!(
                accept
                    .as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending(),
                "no stream should be available yet"
            );

            abandoned.push(flag);
        }

        done_tx.send(abandoned).ok().expect("client is listening");
    });

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    let abandoned = timeout(Duration::from_secs(10), done_rx)
        .await
        .context("the server never finished registering")??;

    // Every abandoned waker should be uniquely owned by the test again: the shared
    // accept futures are polled with the accept state's own waker, never the caller's,
    // so there is nothing else that could still be holding one.
    let retained = abandoned
        .iter()
        .filter(|flag| Arc::strong_count(flag) > 1)
        .count();
    assert_eq!(
        retained, 0,
        "{retained} of {ABANDONED} abandoned accepters are still parked"
    );

    session.close(0, "bye");
    server_task.abort();
    Ok(())
}

/// Register an accepter, let a second one resolve, and assert the first was woken so
/// it can retry. Deterministic — the client is held on a channel until the first
/// accepter is registered, so there is no race to lose and no sleep to tune.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_accept_wakes_every_accepter() -> Result<()> {
    let (chain, key) = make_self_signed()?;

    let mut server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    // Held until the first accepter has registered its waker.
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.expect("a connection");
        let session = request.ok().await.expect("an established session");

        // The first accepter registers, then parks. No stream can have arrived yet:
        // the client is still waiting on the channel.
        let flag = Arc::new(FlagWaker::default());
        let waker = Waker::from(flag.clone());
        let mut first = std::pin::pin!(session.accept_uni());
        assert!(
            first
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending(),
            "no stream should be available yet"
        );

        registered_tx.send(()).expect("client is listening");

        // The second accepter registers over the top of the first and resolves.
        let mut recv = session.accept_uni().await.expect("a stream");
        let mut data = Vec::new();
        recv.read_to_end(&mut data).await.expect("the payload");
        assert_eq!(data, b"hi");

        assert!(
            flag.woken(),
            "the first accepter's waker was dropped, so it would never retry"
        );

        done_tx.send(()).expect("client is listening");
    });

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    registered_rx.await?;

    let mut send = session.open_uni().await?;
    send.write_all(b"hi").await?;
    send.finish()?;

    timeout(Duration::from_secs(10), done_rx)
        .await
        .context("the server never confirmed: the first accepter was not woken")??;

    session.close(0, "bye");
    server_task.abort();
    Ok(())
}

/// The user-visible form of the same bug, matching the original reproducer that
/// diagnosed it for quinn: two independent tasks each awaiting `accept_uni`, and two
/// streams to serve them. Without the waker list, whichever task registered first is
/// never woken again and hangs forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_tasks_can_accept_concurrently() -> Result<()> {
    let (chain, key) = make_self_signed()?;

    let mut server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.expect("a connection");
        let session = request.ok().await.expect("an established session");

        // Two accepters on two clones, exactly as an application would fan out.
        let mut accepters = Vec::new();
        for _ in 0..2 {
            let session = session.clone();
            accepters.push(tokio::spawn(async move {
                let mut recv = session.accept_uni().await.expect("a stream");
                let mut data = Vec::new();
                recv.read_to_end(&mut data).await.expect("the payload");
                data
            }));
        }

        for accepter in accepters {
            assert_eq!(accepter.await.expect("accepter task"), b"hi");
        }

        done_tx.send(()).expect("client is listening");
    });

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    for _ in 0..2 {
        let mut send = session.open_uni().await?;
        send.write_all(b"hi").await?;
        send.finish()?;
    }

    timeout(Duration::from_secs(10), done_rx)
        .await
        .context("an accepter was never woken")??;

    session.close(0, "bye");
    server_task.abort();
    Ok(())
}

/// The last accepter to poll is the one the inner accept path holds a waker for. If it
/// gives up, the arrival still has to reach whoever is left waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_departing_accepter_does_not_strand_the_others() -> Result<()> {
    let (chain, key) = make_self_signed()?;

    let mut server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    // Held until both accepters have registered, so the stream cannot arrive early.
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.expect("a connection");
        let session = request.ok().await.expect("an established session");

        // A parks first.
        let a = Arc::new(FlagWaker::default());
        let a_waker = Waker::from(a.clone());
        let mut first = std::pin::pin!(session.accept_uni());
        assert!(first
            .as_mut()
            .poll(&mut Context::from_waker(&a_waker))
            .is_pending());

        // B polls after A, then gives up.
        {
            let b = Waker::from(Arc::new(FlagWaker::default()));
            let mut second = std::pin::pin!(session.accept_uni());
            assert!(second
                .as_mut()
                .poll(&mut Context::from_waker(&b))
                .is_pending());
        }

        registered_tx.send(()).expect("client is listening");

        // Nothing here polls the accept path again: A has to be woken by the arrival.
        for _ in 0..500 {
            if a.woken() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(a.woken(), "the remaining accepter was never woken");

        done_tx.send(()).expect("client is listening");
    });

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    registered_rx.await?;

    let mut send = session.open_uni().await?;
    send.write_all(b"hi").await?;
    send.finish()?;

    timeout(Duration::from_secs(10), done_rx)
        .await
        .context("the remaining accepter was never woken")??;

    session.close(0, "bye");
    server_task.abort();
    Ok(())
}
