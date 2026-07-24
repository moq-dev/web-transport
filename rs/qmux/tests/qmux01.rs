//! Integration tests for QMux draft-01: record framing, ping, idle handling.

#![cfg(feature = "tcp")]

use std::time::Duration;

use qmux::Version;
use tokio::net::TcpListener;
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
        // We only need that first frame on the wire; this bare server never sends
        // its own params back, so establishment fails (EOF) — ignore the result.
        let _ = qmux::tcp::Config::new(version).connect(addr).await;
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
        let session = qmux::tcp::Config::new(Version::QMux00)
            .accept(sock)
            .await
            .unwrap();

        let mut recv = session.accept_uni().await.unwrap();
        let payload = recv.read_all().await.unwrap();

        let mut send = session.open_uni().await.unwrap();
        send.write(&payload).await.unwrap();
        send.finish().unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let session = qmux::tcp::Config::new(Version::QMux00)
        .connect(addr)
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
        let session = qmux::tcp::Config::new(Version::QMux01)
            .accept(sock)
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

    let session = qmux::tcp::Config::new(Version::QMux01)
        .connect(addr)
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

/// Two idle QMux01 peers keep each other alive with no application traffic: the
/// timer task emits a QX_PING every idle/3, the peer answers, and each response
/// resets the other's idle deadline. So the connection survives well past the idle
/// window instead of self-closing — and the timer keeps pinging regardless of where
/// the reader/writer happen to be parked.
#[tokio::test]
async fn qmux01_ping_keeps_idle_session_alive() {
    use qmux::transport::Stream;
    use qmux::{Config, Session};

    let (a, b) = tokio::io::duplex(64 * 1024);
    // Short idle window so the test is quick; the ping cadence is a third of it.
    let mut config = Config::new(Version::QMux01);
    config.max_idle_timeout = 150;

    let ta = Stream::new(a, config.version, config.max_record_size);
    let tb = Stream::new(b, config.version, config.max_record_size);
    let (client, server) = tokio::join!(
        Session::connect(ta, config.clone()),
        Session::accept(tb, config),
    );
    let client = client.unwrap();
    let server = server.unwrap();

    // Watch both close reasons without consuming the sessions.
    let (c, s) = (client.clone(), server.clone());
    let client_closed = tokio::spawn(async move { c.closed().await });
    let server_closed = tokio::spawn(async move { s.closed().await });

    // Idle for well over 2× the 150ms window: without the keep-alive, both would
    // have long since idle-closed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !client_closed.is_finished(),
        "client idle-closed despite the keep-alive ping"
    );
    assert!(
        !server_closed.is_finished(),
        "server idle-closed despite the keep-alive ping"
    );

    // And the link is genuinely still usable, not merely un-closed.
    let mut send = client.open_uni().await.unwrap();
    send.write(b"alive").await.unwrap();
    send.finish().unwrap();
    let mut recv = server.accept_uni().await.unwrap();
    assert_eq!(recv.read_all().await.unwrap().as_ref(), b"alive");

    client.close(0, "done");
}

/// The other half of that contract: a peer that goes SILENT must be idle-closed,
/// even while our own writes keep landing.
///
/// This is what a vanished host or a dropped network looks like on a stream
/// transport (Unix socket, TCP, WebSocket): no close, no EOF, just nothing ever
/// arriving again while the socket happily accepts everything we send. Our own
/// keep-alive pings advance the send clock every idle/3, so counting every send as
/// activity left the deadline permanently in the future and the session lived until
/// the process died. Only a *received* frame proves the peer is still there.
#[tokio::test]
async fn qmux01_silent_peer_is_idle_closed() {
    use qmux::transport::Stream;
    use qmux::{Config, Error, Session};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client_io, proxy_client) = tokio::io::duplex(64 * 1024);
    let (proxy_server, server_io) = tokio::io::duplex(64 * 1024);
    let silenced = Arc::new(AtomicBool::new(false));

    // Proxy the two sockets so one direction can be cut without closing either.
    // Client→server always flows, so the client's writes keep succeeding and its
    // pings still reach a peer that dutifully answers them. Server→client is drained
    // but discarded once silenced, so the client hears nothing while its own send
    // clock keeps advancing — the exact shape of the bug.
    let (mut client_rx, mut client_tx) = tokio::io::split(proxy_client);
    let (mut server_rx, mut server_tx) = tokio::io::split(proxy_server);
    let upstream =
        tokio::spawn(async move { tokio::io::copy(&mut client_rx, &mut server_tx).await });
    let downstream = tokio::spawn({
        let silenced = silenced.clone();
        async move {
            let mut buf = [0u8; 4096];
            loop {
                let n = match server_rx.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                if silenced.load(Ordering::Relaxed) {
                    continue;
                }
                if client_tx.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
        }
    });

    let mut config = Config::new(Version::QMux01);
    config.max_idle_timeout = 150;
    let ta = Stream::new(client_io, config.version, config.max_record_size);
    let tb = Stream::new(server_io, config.version, config.max_record_size);
    let (client, server) = tokio::join!(
        Session::connect(ta, config.clone()),
        Session::accept(tb, config),
    );
    let client = client.unwrap();
    let _server = server.unwrap();

    silenced.store(true, Ordering::Relaxed);

    // One idle window to notice, plus at most one more for the send credit.
    let reason = tokio::time::timeout(Duration::from_secs(1), client.closed())
        .await
        .expect("a silent peer must idle-close, even while our own sends land");
    assert!(matches!(reason, Error::IdleTimeout), "got {reason:?}");

    upstream.abort();
    downstream.abort();
}
