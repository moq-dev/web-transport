//! End-to-end datagram tests (RFC 9221) over the TCP transport.

#![cfg(feature = "tcp")]

use bytes::Bytes;
use qmux::{transport::Stream, Config, Error, Session, Version};
use tokio::net::{TcpListener, TcpStream};
use web_transport_trait::Session as _;

/// Pair a client and server over TCP loopback with the given configs, awaiting
/// establishment on both ends.
async fn pair(client_cfg: Config, server_cfg: Config) -> (Session, Session) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let transport = Stream::new(sock, server_cfg.version, server_cfg.max_record_size);
        Session::accept(transport, server_cfg).await.unwrap()
    });

    let sock = TcpStream::connect(addr).await.unwrap();
    let transport = Stream::new(sock, client_cfg.version, client_cfg.max_record_size);
    let client = Session::connect(transport, client_cfg).await.unwrap();

    (client, server.await.unwrap())
}

/// Datagrams flow in both directions and arrive intact.
#[tokio::test]
async fn round_trip() {
    let (client, server) = pair(Config::new(Version::QMux01), Config::new(Version::QMux01)).await;

    assert!(client.max_datagram_size() > 0);
    assert!(server.max_datagram_size() > 0);

    client.send_datagram(Bytes::from_static(b"ping")).unwrap();
    assert_eq!(server.recv_datagram().await.unwrap().as_ref(), b"ping");

    server.send_datagram(Bytes::from_static(b"pong")).unwrap();
    assert_eq!(client.recv_datagram().await.unwrap().as_ref(), b"pong");
}

/// Datagrams are a QMux01 feature; on QMux00 they're unsupported in both
/// directions (the parameter isn't advertised, so nothing negotiates them).
#[tokio::test]
async fn disabled_on_qmux00() {
    let (client, server) = pair(Config::new(Version::QMux00), Config::new(Version::QMux00)).await;

    assert_eq!(client.max_datagram_size(), 0);
    assert_eq!(server.max_datagram_size(), 0);
    assert!(matches!(
        client.send_datagram(Bytes::from_static(b"hello")),
        Err(Error::DatagramsUnsupported)
    ));
}

/// A peer that advertises `max_datagram_frame_size = 0` disables datagrams in
/// its receive direction only; the reverse direction still works.
#[tokio::test]
async fn disabled_by_peer_is_one_directional() {
    let mut client_cfg = Config::new(Version::QMux01);
    client_cfg.max_datagram_frame_size = 0; // client won't receive datagrams

    let (client, server) = pair(client_cfg, Config::new(Version::QMux01)).await;

    // The server cannot send to a client that advertised no datagram support.
    assert_eq!(server.max_datagram_size(), 0);
    assert!(matches!(
        server.send_datagram(Bytes::from_static(b"x")),
        Err(Error::DatagramsUnsupported)
    ));

    // The client can still send to the server, which enabled datagrams.
    assert!(client.max_datagram_size() > 0);
    client.send_datagram(Bytes::from_static(b"hey")).unwrap();
    assert_eq!(server.recv_datagram().await.unwrap().as_ref(), b"hey");
}

/// A payload larger than `max_datagram_size()` is rejected before it hits the
/// wire rather than silently truncated.
#[tokio::test]
async fn oversized_payload_rejected() {
    let (client, _server) = pair(Config::new(Version::QMux01), Config::new(Version::QMux01)).await;

    let too_big = vec![0u8; client.max_datagram_size() + 1];
    assert!(matches!(
        client.send_datagram(Bytes::from(too_big)),
        Err(Error::FrameTooLarge)
    ));
}
