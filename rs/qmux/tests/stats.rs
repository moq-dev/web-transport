//! Stream statistics follow protocol state, independently of retained handles.

use qmux::{transport::Stream, Config, Session, Version};
use std::{future::Future, task::Poll};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _, Stats as _};

async fn pair(version: Version) -> (Session, Session) {
    let mut config = Config::new(version);
    config.max_streams_bidi = 1;
    config.max_streams_uni = 1;
    #[cfg(feature = "ws")]
    if version == Version::WebTransport {
        use qmux::ws::{
            tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream},
            Upgraded,
        };
        let (a, b) = tokio::io::duplex(64 * 1024);
        let a = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
        let b = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
        let a = Upgraded::new(a)
            .with_session_config(config.clone())
            .connect();
        let b = Upgraded::new(b).with_session_config(config).accept();
        return (a, b);
    }
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

#[path = "../../web-transport-trait/tests/support/stream_stats.rs"]
mod stream_stats;

#[tokio::test(start_paused = true)]
async fn stream_payload_statistics() {
    for version in [
        Version::QMux00,
        Version::QMux01,
        Version::QMux02,
        Version::WebTransport,
    ] {
        let (client, server) = pair(version).await;
        stream_stats::payload_stats(&client, &server).await;
        client.close(0, "done");
        server.close(0, "done");
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_buffer_write_counts_only_accepted_payload() {
    let mut config = Config::new(Version::QMux02);
    config.max_stream_data_uni = 3;
    let (a, b) = tokio::io::duplex(64 * 1024);
    let a = Stream::new(a, config.version, config.max_record_size);
    let b = Stream::new(b, config.version, config.max_record_size);
    let (client, server) = tokio::join!(
        Session::connect(a, config.clone()),
        Session::accept(b, config)
    );
    let (client, server) = (client.unwrap(), server.unwrap());
    let mut send = client.open_uni().await.unwrap();
    let mut data = bytes::Bytes::from_static(b"abcdef");
    {
        let mut write = std::pin::pin!(send.write_all_buf(&mut data));
        std::future::poll_fn(|cx| {
            assert!(write.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert_eq!(data.as_ref(), b"def");
    let stats = send.stats().await.unwrap();
    assert_eq!(stats.bytes_written, Some(3));
    assert_eq!(stats.bytes_sent, None);
    assert_eq!(stats.bytes_acknowledged, None);
    send.reset(0);
    assert_eq!(send.stats().await.unwrap(), stats);
    client.close(0, "done");
    server.close(0, "done");
}
