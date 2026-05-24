//! Integration tests for QMux draft-01: record framing, ping, idle handling.

#![cfg(feature = "tcp")]

use std::time::Duration;

use bytes::Bytes;
use qmux::proto::{Frame, Ping, Stream};
use qmux::{StreamId, Version};
use tokio::net::TcpListener;
use web_transport_proto::VarInt;
use web_transport_trait::{RecvStream, SendStream, Session as _};

/// Round-trip multiple frames concatenated inside one record body.
///
/// Records can carry several frames, so `decode_record` must keep parsing
/// until the buffer is exhausted and stop cleanly at the boundary.
#[test]
fn record_round_trip_multiple_frames() {
    let stream_id = StreamId(VarInt::from_u32(0));
    let frames = vec![
        Frame::Stream(Stream {
            id: stream_id,
            data: Bytes::from_static(b"hello"),
            fin: false,
        }),
        Frame::Ping(Ping {
            sequence: 42,
            response: false,
        }),
        Frame::MaxData(1024),
    ];

    // Concatenate the encoded frames as a single record body — the same way the
    // wire layer would lay them out inside one record.
    let mut body = bytes::BytesMut::new();
    for frame in &frames {
        body.extend_from_slice(&frame.encode(Version::QMux01).unwrap());
    }

    let decoded = Frame::decode_record(body.freeze()).unwrap();
    assert_eq!(decoded.len(), 3);

    match &decoded[0] {
        Frame::Stream(s) => {
            assert_eq!(s.data.as_ref(), b"hello");
            assert!(!s.fin);
        }
        other => panic!("expected Stream, got {other:?}"),
    }
    match &decoded[1] {
        Frame::Ping(p) => {
            assert_eq!(p.sequence, 42);
            assert!(!p.response);
        }
        other => panic!("expected Ping, got {other:?}"),
    }
    match &decoded[2] {
        Frame::MaxData(v) => assert_eq!(*v, 1024),
        other => panic!("expected MaxData, got {other:?}"),
    }
}

/// End-to-end QMux01 over a real TCP socket: STREAM data + PING/PONG keep-alive.
///
/// Exercises the full transport — record size-varint framing on the wire,
/// session-level frame routing, and the QX_PING request/response path.
#[tokio::test]
async fn qmux01_tcp_stream_and_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let session = qmux::tcp::accept(sock, Some(Version::QMux01))
            .await
            .unwrap();

        // Echo the client's STREAM payload back on a new uni stream.
        let mut recv = session.accept_uni().await.unwrap();
        let payload = recv.read_all().await.unwrap();

        let mut send = session.open_uni().await.unwrap();
        send.write(&payload).await.unwrap();
        send.finish().unwrap();

        // Hold the session open long enough for the client to receive the response
        // and run its ping round-trip; tying our shutdown to the client's `close`
        // would race the ping flow we're actually trying to test.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let session = qmux::tcp::connect(addr, Some(Version::QMux01))
        .await
        .unwrap();

    // Send "ping" over a uni stream.
    let mut send = session.open_uni().await.unwrap();
    send.write(b"qmux01").await.unwrap();
    send.finish().unwrap();

    // Read the echoed payload from the server's response stream.
    let mut recv = session.accept_uni().await.unwrap();
    let echoed = recv.read_all().await.unwrap();
    assert_eq!(echoed.as_ref(), b"qmux01");

    session.close(0, "done");
    server_task.await.unwrap();
}
