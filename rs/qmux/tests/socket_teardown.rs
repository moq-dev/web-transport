//! Retained session handles must not retain the socket statistics descriptor.
#![cfg(all(unix, feature = "tcp"))]

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use qmux::{Config, Session, SharedSocketStats, SocketStats, TcpStats, Version};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Notify};
use web_transport_trait::{SendStream as _, Session as _, Stats as _};

// Fields drop in declaration order: the notification follows descriptor release.
struct TrackedStats {
    tcp: TcpStats,
    _released: oneshot::Sender<()>,
}

impl SocketStats for TrackedStats {
    fn rtt(&self) -> Option<Duration> {
        self.tcp.rtt()
    }

    fn estimated_send_rate(&self) -> Option<u64> {
        self.tcp.estimated_send_rate()
    }
}

fn tracked(socket: &TcpStream) -> (SharedSocketStats, oneshot::Receiver<()>) {
    let (released, done) = oneshot::channel();
    (
        Arc::new(TrackedStats {
            tcp: TcpStats::new(socket).unwrap(),
            _released: released,
        }),
        done,
    )
}

async fn sockets() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept(),
    );
    (client.unwrap(), server.unwrap().0)
}

async fn released(done: oneshot::Receiver<()>, session: &Session) {
    tokio::time::timeout(Duration::from_secs(2), done)
        .await
        .expect("retained Session kept the socket stats descriptor alive")
        .expect_err("the sender only signals by dropping");
    assert_eq!(session.stats().rtt(), None);
    assert_eq!(session.stats().estimated_send_rate(), None);
}

fn active(session: &Session) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(session.stats().rtt().is_some());
    // Stream counters remain available independently of socket stats.
    assert!(session.stats().streams().is_some());
}

#[tokio::test]
async fn tcp_close_releases_stats_with_retained_clones() {
    let (client, server) = sockets().await;
    let (client_stats, client_done) = tracked(&client);
    let (server_stats, server_done) = tracked(&server);
    let mut config = Config::new(Version::QMux02);
    // Isolate socket RTT from the QMux estimate, which survives teardown.
    config.rtt_interval = Duration::ZERO;
    let (client, server) = tokio::join!(
        Session::connect(
            qmux::transport::Stream::new(client, config.version, config.max_record_size)
                .with_socket_stats(client_stats),
            config.clone(),
        ),
        Session::accept(
            qmux::transport::Stream::new(server, config.version, config.max_record_size)
                .with_socket_stats(server_stats),
            config.clone(),
        ),
    );
    let client = client.unwrap();
    let server = server.unwrap();
    active(&client);
    active(&server);
    let retained = client.clone();
    client.close(0, "done");
    drop(client);
    released(client_done, &retained).await;
    released(server_done, &server).await;
    assert!(retained.stats().streams().is_some());
    assert!(server.stats().streams().is_some());
}

#[tokio::test]
async fn tcp_peer_eof_releases_stats_with_retained_clone() {
    let (client, mut peer) = sockets().await;
    let (stats, done) = tracked(&client);
    let config = Config::new(Version::WebTransport);
    let client = Session::connect(
        qmux::transport::Stream::new(client, config.version, config.max_record_size)
            .with_socket_stats(stats),
        config,
    )
    .await
    .unwrap();
    let retained = client.clone();
    drop(client);
    peer.shutdown().await.unwrap();
    released(done, &retained).await;
    assert!(matches!(retained.closed().await, qmux::Error::Io(err)
        if err.kind() == std::io::ErrorKind::UnexpectedEof));
}

// Gate actual socket writes so cancellation skips graceful shutdown. Without
// releasing TcpStats, dropping the transport halves alone cannot deliver EOF.
struct GatedSocket {
    socket: TcpStream,
    entered: Arc<Notify>,
}

impl AsyncRead for GatedSocket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_read(cx, buf)
    }
}

impl AsyncWrite for GatedSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.entered.notify_one();
        Poll::Pending
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn tcp_blocked_writer_releases_stats_and_delivers_eof() {
    let (socket, mut peer) = sockets().await;
    let (stats, done) = tracked(&socket);
    let entered = Arc::new(Notify::new());
    let config = Config::new(Version::WebTransport);
    let client = Session::connect(
        qmux::transport::Stream::new(
            GatedSocket {
                socket,
                entered: entered.clone(),
            },
            config.version,
            config.max_record_size,
        )
        .with_socket_stats(stats),
        config,
    )
    .await
    .unwrap();
    let mut send = client.open_uni().await.unwrap();
    send.write(b"blocked").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("writer never blocked");
    let retained = client.clone();
    client.close(0, "done");
    drop(client);
    released(done, &retained).await;
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), peer.read_to_end(&mut bytes))
        .await
        .expect("peer never observed EOF while Session was retained")
        .unwrap();
}

#[cfg(feature = "ws")]
#[tokio::test]
async fn upgraded_websocket_releases_stats_with_retained_clone() {
    use qmux::ws::{tokio_tungstenite, Upgraded};

    for local_close in [false, true] {
        let (client, peer) = sockets().await;
        let (stats, done) = tracked(&client);
        let (client, peer) = tokio::join!(
            tokio_tungstenite::client_async("ws://localhost/", client),
            tokio_tungstenite::accept_async(peer),
        );
        let client = Upgraded::new(client.unwrap().0)
            .with_socket_stats(stats)
            .connect();
        let peer = peer.unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(client.stats().rtt().is_some());
        let retained = client.clone();
        drop(client);
        if local_close {
            retained.close(0, "done");
            released(done, &retained).await;
        } else {
            // An abrupt failure of the upgraded socket must release the duplicate too.
            drop(peer);
            released(done, &retained).await;
        }
    }
}
