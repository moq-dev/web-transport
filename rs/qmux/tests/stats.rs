//! Stream statistics follow protocol state, independently of retained handles.

use qmux::{transport::Stream, Config, Session, Version};
use std::{future::Future, task::Poll};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _, Stats as _};

async fn pair(version: Version) -> (Session, Session) {
    let mut config = Config::new(version);
    config.max_streams_bidi = 1;
    config.max_streams_uni = 1;
    let (a, b) = tokio::io::duplex(64 * 1024);
    let a = Stream::new(a, version, config.max_record_size);
    let b = Stream::new(b, version, config.max_record_size);
    let (a, b) = tokio::join!(
        Session::connect(a, config.clone()),
        Session::accept(b, config)
    );
    (a.unwrap(), b.unwrap())
}

#[tokio::test(start_paused = true)]
async fn occupancy_and_credit_are_distinct_from_handles_and_cumulative_limits() {
    for version in [Version::QMux00, Version::QMux01, Version::QMux02] {
        let (client, server) = pair(version).await;
        let initial = client.stats().streams().unwrap();
        assert_eq!(initial.uni.open, 0);
        assert_eq!(initial.uni.available, 1);

        let mut uni = client.open_uni().await.unwrap();
        uni.write(b"one").await.unwrap();
        let saved = client.stats();
        assert_eq!(saved.streams().unwrap().uni.open, 1);
        assert_eq!(saved.streams().unwrap().uni.available, 0);
        let mut received = server.accept_uni().await.unwrap();
        let mut waiting = std::pin::pin!(client.open_uni());
        std::future::poll_fn(|cx| {
            assert!(
                waiting.as_mut().poll(cx).is_pending(),
                "exhausted credit allowed an open"
            );
            Poll::Ready(())
        })
        .await;
        uni.finish().unwrap();
        assert_eq!(received.read_all().await.unwrap().as_ref(), b"one");
        assert_eq!(client.stats().streams().unwrap().uni.open, 0);
        // Retaining the completed frontend does not keep a transport slot open.
        drop(received);
        let mut next = waiting.await.unwrap();
        assert_eq!(client.stats().streams().unwrap().uni.available, 0);
        assert_eq!(client.stats().streams().unwrap().uni.open, 1);
        // The peer granted a second cumulative stream ID, not a capacity of two.
        assert_eq!(saved.streams().unwrap().uni.open, 1);
        assert_eq!(saved.streams().unwrap().uni.available, 0);
        next.write(b"two").await.unwrap();
        let mut received = server.accept_uni().await.unwrap();
        next.reset(7);
        assert!(received.read_all().await.is_err());
        assert_eq!(client.stats().streams().unwrap().uni.open, 0);

        let (mut send, mut recv) = client.open_bi().await.unwrap();
        send.write(b"request").await.unwrap();
        let (mut reply, mut request) = server.accept_bi().await.unwrap();
        assert_eq!(client.stats().streams().unwrap().bidi.open, 1);
        assert_eq!(server.stats().streams().unwrap().bidi.open, 0);
        send.finish().unwrap();
        request.read_all().await.unwrap();
        assert_eq!(client.stats().streams().unwrap().bidi.open, 1);
        reply.finish().unwrap();
        recv.read_all().await.unwrap();
        assert_eq!(client.stats().streams().unwrap().bidi.open, 0);
        client.close(0, "done");
        assert_eq!(
            client.stats().streams().unwrap(),
            web_transport_trait::Streams::default()
        );
        server.close(0, "done");
    }
}

#[tokio::test(start_paused = true)]
async fn legacy_format_does_not_invent_stream_credit() {
    let (client, server) = pair(Version::WebTransport).await;
    assert!(client.stats().streams().is_none());
    assert!(web_transport_trait::StatsUnavailable.streams().is_none());
    client.close(0, "done");
    server.close(0, "done");
}
