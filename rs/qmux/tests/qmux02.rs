//! Integration tests for QMux draft-02 over a real TCP socket.
//!
//! Draft-02 shares the draft-01 record wire format, so this focuses on the
//! end-to-end happy path; the draft-02-specific validation rules (RESET_STREAM_AT,
//! QX_TRANSPORT_PARAMETERS ordering, QX_PING sequence numbers, max_record_size
//! minimum) are covered by in-crate raw-peer tests in `src/session.rs` and
//! byte-level fixtures in `tests/wire_format.rs`.

#![cfg(feature = "tcp")]

use std::time::Duration;

use qmux::Version;
use tokio::net::TcpListener;
use web_transport_trait::{RecvStream, SendStream, Session as _};

/// End-to-end QMux02 over TCP: open a stream, echo it back, close.
#[tokio::test]
async fn qmux02_tcp_stream_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let session = qmux::tcp::Config::new(Version::QMux02)
            .accept(sock)
            .await
            .unwrap();

        let mut recv = session.accept_uni().await.unwrap();
        let payload = recv.read_all().await.unwrap();

        let mut send = session.open_uni().await.unwrap();
        send.write(&payload).await.unwrap();
        send.finish().unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    });

    let session = qmux::tcp::Config::new(Version::QMux02)
        .connect(addr)
        .await
        .unwrap();
    let mut send = session.open_uni().await.unwrap();
    send.write(b"qmux02").await.unwrap();
    send.finish().unwrap();

    let mut recv = session.accept_uni().await.unwrap();
    let echoed = recv.read_all().await.unwrap();
    assert_eq!(echoed.as_ref(), b"qmux02");

    session.close(0, "done");
    server.await.unwrap();
}

/// A default-config session negotiates QMux02: `Config::default()` selects the
/// newest draft, and two defaulted peers interoperate over TCP.
#[tokio::test]
async fn default_config_uses_qmux02() {
    assert_eq!(qmux::Config::default().version, Version::QMux02);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let session = qmux::tcp::Config::new(qmux::Config::default().version)
            .accept(sock)
            .await
            .unwrap();
        let mut recv = session.accept_uni().await.unwrap();
        assert_eq!(recv.read_all().await.unwrap().as_ref(), b"default");
    });

    let session = qmux::tcp::Config::new(qmux::Config::default().version)
        .connect(addr)
        .await
        .unwrap();
    let mut send = session.open_uni().await.unwrap();
    send.write(b"default").await.unwrap();
    send.finish().unwrap();

    server.await.unwrap();
    session.close(0, "done");
}

/// Two idle QMux02 peers keep each other alive with QX_PING, exercising the
/// draft-02 sequence validation end-to-end: each side's timer emits strictly
/// increasing requests and echoes the peer's, and neither the strict-increase
/// (requests) nor the echo-bound (responses) check wrongly closes the session.
/// A regression here would surface as a spurious PROTOCOL_VIOLATION close.
#[tokio::test]
async fn qmux02_ping_keeps_idle_session_alive() {
    use qmux::transport::Stream;
    use qmux::{Config, Session};

    let (a, b) = tokio::io::duplex(64 * 1024);
    let mut config = Config::new(Version::QMux02);
    config.max_idle_timeout = 150; // short window; ping cadence is a third of it

    let ta = Stream::new(a, config.version, config.max_record_size);
    let tb = Stream::new(b, config.version, config.max_record_size);
    let (client, server) = tokio::join!(
        Session::connect(ta, config.clone()),
        Session::accept(tb, config),
    );
    let client = client.unwrap();
    let server = server.unwrap();

    let (c, s) = (client.clone(), server.clone());
    let client_closed = tokio::spawn(async move { c.closed().await });
    let server_closed = tokio::spawn(async move { s.closed().await });

    // Idle for well over 2× the window: without keep-alive both would idle-close,
    // and a broken ping-sequence check would PROTOCOL_VIOLATION-close instead.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!client_closed.is_finished(), "client closed while idle");
    assert!(!server_closed.is_finished(), "server closed while idle");

    // Still genuinely usable.
    let mut send = client.open_uni().await.unwrap();
    send.write(b"alive").await.unwrap();
    send.finish().unwrap();
    let mut recv = server.accept_uni().await.unwrap();
    assert_eq!(recv.read_all().await.unwrap().as_ref(), b"alive");

    client.close(0, "done");
}
