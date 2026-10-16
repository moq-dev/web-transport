//! TCP wire counters through an upgraded WebSocket session.

#![cfg(all(feature = "ws", target_os = "linux"))]

use std::sync::Arc;

use qmux::ws::tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};
use qmux::{ws, Session, TcpStats};
use tokio::net::{TcpListener, TcpStream};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _, Stats as _};

async fn pair() -> (Session, Session) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let stats = Arc::new(TcpStats::new(&socket).unwrap());
        let socket = WebSocketStream::from_raw_socket(socket, Role::Server, None).await;
        ws::Upgraded::new(socket)
            .with_alpn("qmux-02.test")
            .with_socket_stats(stats)
            .accept()
    });
    let socket = TcpStream::connect(addr).await.unwrap();
    let stats = Arc::new(TcpStats::new(&socket).unwrap());
    let socket = WebSocketStream::from_raw_socket(socket, Role::Client, None).await;
    let client = ws::Upgraded::new(socket)
        .with_alpn("qmux-02.test")
        .with_socket_stats(stats)
        .connect();
    (client, server.await.unwrap())
}

#[tokio::test]
async fn websocket_reports_cumulative_tcp_bytes_in_both_directions() {
    let (client, server) = pair().await;
    for (sender, receiver) in [(&client, &server), (&server, &client)] {
        let before = sender.stats().bytes_sent().expect("TCP sent counter");
        let payload = vec![0x5a; 32 * 1024];
        let mut stream = sender.open_uni().await.unwrap();
        stream.write(&payload).await.unwrap();
        stream.finish().unwrap();
        let mut stream = receiver.accept_uni().await.unwrap();
        assert_eq!(stream.read_all().await.unwrap().as_ref(), payload);

        let sent = sender.stats().bytes_sent().expect("TCP sent counter");
        let received = receiver
            .stats()
            .bytes_received()
            .expect("TCP receive counter");
        assert!(sent >= before + payload.len() as u64, "{before} -> {sent}");
        // The peer has already delivered the entire payload, so both counters
        // cover it now without waiting for an ACK or another stats sample.
        assert!(received >= payload.len() as u64, "{received}");
    }
    client.close(0, "done");
    server.close(0, "done");
}
