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
