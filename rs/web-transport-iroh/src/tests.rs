use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Wake, Waker},
    time::Duration,
};

use iroh::{
    Endpoint,
    endpoint::{ConnectionError, presets},
};
use n0_tracing_test::traced_test;
use tokio::time::timeout;
use tracing::Instrument;
use url::Url;

use crate::{ALPN_H3, Client, H3Request, QuicRequest, SessionError};

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

#[tokio::test]
#[traced_test]
async fn h3_smoke() -> n0_error::Result<()> {
    let client = Endpoint::bind(presets::Minimal)
        .instrument(tracing::error_span!("client-ep"))
        .await
        .unwrap();
    let client_id = client.id();
    let client = Client::new(client);

    let server = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN_H3.as_bytes().to_vec()])
        .bind()
        .instrument(tracing::error_span!("server-ep"))
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let url: Url = format!("https://{}/foo", server_id).parse().unwrap();

    let client_task = tokio::task::spawn({
        let url = url.clone();
        async move {
            let session = client.connect_h3(server_addr, url.clone()).await.inspect_err(|err| println!("{err:#?}")).unwrap();
            assert_eq!(session.remote_id(), server_id);
            assert_eq!(session.request().map(|r| &r.url), Some(&url));

            let mut stream = session.open_uni().await.unwrap();
            stream.write_all(b"hi").await.unwrap();
            stream.finish().unwrap();
            let reason = session.closed().await;
            assert!(
                matches!(reason, SessionError::ConnectionError(ConnectionError::ApplicationClosed(frame)) if web_transport_proto::error_from_http3(frame.error_code.into_inner()) == Some(23))
            );

            drop(session);
            client.close().await;
        }.instrument(tracing::error_span!("client"))
    });

    let server_task = tokio::task::spawn(
        async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            assert_eq!(conn.alpn(), ALPN_H3.as_bytes());
            let request = H3Request::accept(conn)
                .await
                .inspect_err(|err| tracing::error!("accept failed: {err:?}"))
                .unwrap();
            assert_eq!(request.url, url);
            assert_eq!(request.conn().remote_id(), client_id);
            let session = request.ok().await.unwrap();
            assert_eq!(session.request().map(|r| &r.url), Some(&url));
            assert_eq!(session.conn().remote_id(), client_id);
            let mut stream = session.accept_uni().await.unwrap();
            let buf = stream.read_to_end(2).await.unwrap();
            assert_eq!(buf, b"hi");
            session.close(23, b"bye");
            server.close().await;
        }
        .instrument(tracing::error_span!("server")),
    );

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out")
        .unwrap();

    Ok(())
}

/// Every clone of a session polls one shared `H3SessionAccept`, which used to keep
/// only the most recent waker: a second accepter registered through a waker the
/// first had already replaced, so the first never woke again.
///
/// Register an accepter, let a second one resolve, and assert the first was woken so
/// it can retry. Deterministic — the client is held back by a channel until the
/// first accepter is registered, so there is no race to lose and no sleep to tune.
#[tokio::test]
#[traced_test]
async fn concurrent_accept_wakes_every_accepter() -> n0_error::Result<()> {
    let client = Endpoint::bind(presets::Minimal).await.unwrap();
    let client = Client::new(client);

    let server = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN_H3.as_bytes().to_vec()])
        .bind()
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let url: Url = format!("https://{}/foo", server_id).parse().unwrap();

    // Held until the first accepter has registered its waker.
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();

    let client_task = tokio::task::spawn({
        let url = url.clone();
        async move {
            let session = client.connect_h3(server_addr, url).await.unwrap();

            registered_rx.await.unwrap();

            let mut stream = session.open_uni().await.unwrap();
            stream.write_all(b"hi").await.unwrap();
            stream.finish().unwrap();

            session.closed().await;
            client.close().await;
        }
    });

    let server_task = tokio::task::spawn(async move {
        let conn = server.accept().await.unwrap().await.unwrap();
        let session = H3Request::accept(conn).await.unwrap().ok().await.unwrap();

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

        registered_tx.send(()).unwrap();

        // The second accepter registers over the top of the first and resolves.
        let mut stream = session.accept_uni().await.unwrap();
        assert_eq!(stream.read_to_end(2).await.unwrap(), b"hi");

        assert!(
            flag.woken(),
            "the first accepter's waker was dropped, so it would never retry"
        );

        session.close(0, b"bye");
        server.close().await;
    });

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out")
        .unwrap();

    Ok(())
}

#[tokio::test]
#[traced_test]
async fn quic_smoke() -> n0_error::Result<()> {
    const ALPN: &str = "moql";

    let client = Endpoint::bind(presets::Minimal).await.unwrap();
    let client_id = client.id();
    let client = Client::new(client);

    let server = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN.as_bytes().to_vec()])
        .bind()
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let client_task = tokio::task::spawn({
        async move {
            let session = client
                .connect_quic(server_addr, ALPN.as_bytes())
                .await
                .unwrap();
            println!("session established");
            assert_eq!(session.remote_id(), server_id);
            assert!(session.request().is_none());
            assert_eq!(session.protocol(), Some(ALPN));
            let reason = session.closed().await;
            assert!(
                matches!(reason, SessionError::ConnectionError(ConnectionError::ApplicationClosed(frame)) if frame.error_code.into_inner() == 23)
            );
            client.close().await;
        }.instrument(tracing::error_span!("client"))
    });

    let server_task = tokio::task::spawn({
        async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            assert_eq!(conn.alpn(), ALPN.as_bytes());
            let request = QuicRequest::accept(conn);
            assert_eq!(request.conn().remote_id(), client_id);
            let session = request.ok();
            assert!(session.request().is_none());
            assert_eq!(session.protocol(), Some(ALPN));
            assert_eq!(session.conn().remote_id(), client_id);
            session.close(23, b"bye");
            server.close().await;
        }
        .instrument(tracing::error_span!("server"))
    });

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out")
        .unwrap();

    Ok(())
}

/// The user-visible form of the same bug, matching the original reproducer that
/// diagnosed it for quinn: two independent tasks each awaiting `accept_uni`, and two
/// streams to serve them. Without the waker list, whichever task registered first is
/// never woken again and hangs forever.
#[tokio::test]
#[traced_test]
async fn two_tasks_can_accept_concurrently() -> n0_error::Result<()> {
    let client = Endpoint::bind(presets::Minimal).await.unwrap();
    let client = Client::new(client);

    let server = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN_H3.as_bytes().to_vec()])
        .bind()
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let url: Url = format!("https://{}/foo", server_id).parse().unwrap();

    let client_task = tokio::task::spawn({
        let url = url.clone();
        async move {
            let session = client.connect_h3(server_addr, url).await.unwrap();
            for _ in 0..2 {
                let mut stream = session.open_uni().await.unwrap();
                stream.write_all(b"hi").await.unwrap();
                stream.finish().unwrap();
            }
            session.closed().await;
            client.close().await;
        }
    });

    let server_task = tokio::task::spawn(async move {
        let conn = server.accept().await.unwrap().await.unwrap();
        let session = H3Request::accept(conn).await.unwrap().ok().await.unwrap();

        // Two accepters on two clones, exactly as an application would fan out.
        let mut accepters = Vec::new();
        for _ in 0..2 {
            let session = session.clone();
            accepters.push(tokio::task::spawn(async move {
                let mut recv = session.accept_uni().await.unwrap();
                recv.read_to_end(2).await.unwrap()
            }));
        }

        for accepter in accepters {
            assert_eq!(accepter.await.unwrap(), b"hi");
        }

        session.close(0, b"bye");
        server.close().await;
    });

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out: an accepter was never woken")
        .unwrap();

    Ok(())
}

/// An accepter that walks away must not leave its waker behind.
///
/// `H3SessionAccept` used to keep a `Vec<Waker>` with no way to say "I'm gone": it was
/// only ever cleared by a stream arriving or the connection failing. A caller that
/// started an accept and dropped the future — a `timeout` around `accept_uni()`, say —
/// left its waker there forever, so distinct tasks doing that on a quiet connection grew
/// the list without bound, retaining a task allocation apiece.
///
/// No stream is ever delivered here: an arrival drains the list, which would hide
/// exactly what is under test.
#[tokio::test]
#[traced_test]
async fn abandoned_accepters_release_their_wakers() -> n0_error::Result<()> {
    /// Well past any plausible bound on concurrent accepters.
    const ABANDONED: usize = 256;

    let client = Endpoint::bind(presets::Minimal).await.unwrap();
    let client = Client::new(client);

    let server = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN_H3.as_bytes().to_vec()])
        .bind()
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let url: Url = format!("https://{}/foo", server_id).parse().unwrap();

    // The client holds the connection open until the server has finished checking, and
    // the server waits for the handshake to complete before it starts.
    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    let client_task = tokio::task::spawn(async move {
        let session = client.connect_h3(server_addr, url).await.unwrap();
        connected_tx.send(()).unwrap();
        done_rx.await.unwrap();
        session.close(0, b"bye");
        client.close().await;
    });

    let server_task = tokio::task::spawn(async move {
        let conn = server.accept().await.unwrap().await.unwrap();
        let session = H3Request::accept(conn).await.unwrap().ok().await.unwrap();
        connected_rx.await.unwrap();

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

        // Every abandoned waker should be uniquely owned by this test again. The inner
        // accept future holds the shared waker, not the caller's, so there is nothing to
        // displace first.
        let retained = abandoned
            .iter()
            .filter(|flag| Arc::strong_count(flag) > 1)
            .count();
        assert_eq!(
            retained, 0,
            "{retained} of {ABANDONED} abandoned accepters are still parked"
        );

        done_tx.send(()).unwrap();
        server.close().await;
    });

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out")
        .unwrap();

    Ok(())
}
