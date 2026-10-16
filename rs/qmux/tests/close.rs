//! Terminal sessions must release stream waiters while handles remain alive.

#![cfg(feature = "tcp")]

use std::{future::Future, task::Poll};

use qmux::{Error, Version};
use tokio::net::TcpListener;
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut future = std::pin::pin!(future);
    future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

async fn pair(version: Version) -> (qmux::Session, qmux::Session) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = qmux::tcp::Config::new(version);
    let (client, server) = tokio::join!(config.clone().connect(addr), async {
        let (socket, _) = listener.accept().await.unwrap();
        config.accept(socket).await
    });
    (client.unwrap(), server.unwrap())
}

#[tokio::test]
async fn tcp_terminal_session_drains_unfinished_receive_stream() {
    for version in [Version::QMux01, Version::QMux02] {
        let (client, server) = pair(version).await;
        let mut send = client.open_uni().await.unwrap();
        send.write(b"buffered").await.unwrap();
        let mut recv = server.accept_uni().await.unwrap();
        assert_eq!(recv.read_chunk(2).await.unwrap().unwrap(), b"bu".as_slice());

        client.close(42, "done");
        server.closed().await;
        // This is a teardown barrier, independent of scheduling and time.
        // Keep both Session and stream handles alive beyond backend shutdown.
        assert!(server.accept_uni().await.is_err());
        assert_eq!(
            recv.read_chunk(64).await.unwrap().unwrap(),
            b"ffered".as_slice()
        );
        assert!(matches!(
            poll_once(recv.read_chunk(64)),
            Poll::Ready(Err(Error::Closed))
        ));
        assert!(matches!(
            poll_once(recv.closed()),
            Poll::Ready(Err(Error::Closed))
        ));
    }
}

#[tokio::test]
async fn tcp_terminal_session_closes_unfinished_bidirectional_stream() {
    for version in [Version::QMux01, Version::QMux02] {
        let (client, server) = pair(version).await;
        let (mut send, _recv) = client.open_bi().await.unwrap();
        send.write(b"hello").await.unwrap();
        let (mut send, mut recv) = server.accept_bi().await.unwrap();
        assert_eq!(
            recv.read_chunk(64).await.unwrap().unwrap(),
            b"hello".as_slice()
        );

        client.close(42, "done");
        server.closed().await;
        assert!(server.accept_bi().await.is_err());
        assert!(matches!(
            poll_once(send.closed()),
            Poll::Ready(Err(Error::Closed))
        ));
        assert!(matches!(
            poll_once(recv.read_chunk(64)),
            Poll::Ready(Err(Error::Closed))
        ));
    }
}
