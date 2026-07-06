//! Integration tests for QMux draft-02 over a real TCP socket.
//!
//! Draft-02 shares the draft-01 record wire format, so this focuses on the
//! end-to-end happy path; the draft-02-specific validation rules (RESET_STREAM_AT,
//! QX_TRANSPORT_PARAMETERS ordering, QX_PING sequence numbers, max_record_size
//! minimum) are covered by in-crate raw-peer tests in `src/session.rs` and
//! byte-level fixtures in `tests/wire_format.rs`.

#![cfg(feature = "tcp")]

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
