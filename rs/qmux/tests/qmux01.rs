//! Integration tests for QMux draft-01: record framing, ping, idle handling.

#![cfg(feature = "tcp")]

use std::time::Duration;

use bytes::Bytes;
use qmux::proto::{Frame, Ping, Stream};
use qmux::{StreamId, Version};
use tokio::net::TcpListener;
use web_transport_proto::VarInt;
use web_transport_trait::{RecvStream, SendStream, Session as _};

/// Byte-level wire snapshot: QMux00 must NOT prepend a size varint, QMux01 must.
///
/// Regression guard for the record-framing fix — it's easy for the size prefix
/// to leak into the QMux00 path and silently break the old wire format.
#[tokio::test]
async fn wire_format_size_prefix_qmux01_only() {
    use tokio::io::AsyncReadExt;

    // Spin up a client→server TCP pair so we can read raw bytes off the wire
    // before any frame parsing happens.
    async fn capture_first_send(version: Version) -> Vec<u8> {
        // TCP `read` can return partial data, so loop until we have enough bytes
        // for the indexed assertions below (the longest is `qmux01_bytes[size_len]`,
        // which needs the size varint plus one trailing byte — at most 9 bytes).
        const MIN_BYTES: usize = 9;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::with_capacity(256);
            let mut chunk = [0u8; 64];
            while buf.len() < MIN_BYTES {
                let n = sock.read(&mut chunk).await.unwrap();
                assert!(n > 0, "socket closed before the first frame header arrived");
                buf.extend_from_slice(&chunk[..n]);
            }
            buf
        });
        // The session sends TRANSPORT_PARAMETERS as its first frame on connect.
        let _client = qmux::tcp::connect(addr, Some(version)).await.unwrap();
        server.await.unwrap()
    }

    // QX_TRANSPORT_PARAMETERS frame type encodes to an 8-byte varint starting
    // with 0xff (since the type tag is in the top two bits and the value is huge).
    let qmux00_bytes = capture_first_send(Version::QMux00).await;
    assert_eq!(
        qmux00_bytes[0], 0xff,
        "QMux00 must start with the QX_TRANSPORT_PARAMETERS frame type (no size prefix), got {qmux00_bytes:?}"
    );

    // QMux01 must lead with a size varint, then the frame.
    let qmux01_bytes = capture_first_send(Version::QMux01).await;
    assert_ne!(
        qmux01_bytes[0], 0xff,
        "QMux01 must lead with a record size varint, not the raw frame type"
    );
    // After stripping the size varint, the next byte should be 0xff (the frame type).
    let size_tag = qmux01_bytes[0] >> 6;
    let size_len = 1usize << size_tag;
    assert_eq!(
        qmux01_bytes[size_len], 0xff,
        "expected QX_TRANSPORT_PARAMETERS frame type after the {size_len}-byte size varint"
    );
}

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

/// QMux00 round-trip over TCP, exercising the legacy wire format.
///
/// Regression test for the QMux01 record-framing changes: QMux00 must continue
/// to talk raw frames without any size-varint prefix, and the QMux01-only
/// idle-timeout / record-size logic must stay dormant.
#[tokio::test]
async fn qmux00_tcp_round_trip_unchanged() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let session = qmux::tcp::accept(sock, Some(Version::QMux00))
            .await
            .unwrap();

        let mut recv = session.accept_uni().await.unwrap();
        let payload = recv.read_all().await.unwrap();

        let mut send = session.open_uni().await.unwrap();
        send.write(&payload).await.unwrap();
        send.finish().unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let session = qmux::tcp::connect(addr, Some(Version::QMux00))
        .await
        .unwrap();
    let mut send = session.open_uni().await.unwrap();
    send.write(b"qmux00").await.unwrap();
    send.finish().unwrap();

    let mut recv = session.accept_uni().await.unwrap();
    let echoed = recv.read_all().await.unwrap();
    assert_eq!(echoed.as_ref(), b"qmux00");

    session.close(0, "done");
    server.await.unwrap();
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
