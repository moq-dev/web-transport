//! Round-trip time reported from QX_PING exchanges, and from the kernel over TCP.

#![cfg(feature = "tcp")]

use std::time::Duration;

use qmux::{transport::Stream, Config, Session, Version};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use web_transport_trait::{SendStream as _, Session as _, Stats as _};

/// One-way latency of the in-memory link, so every QX_PING sample is exactly twice it.
const LATENCY: Duration = Duration::from_millis(10);

/// The probe interval the protocol tests configure.
const INTERVAL: Duration = Duration::from_millis(20);

/// Virtual time for the first probe to go out and its response to return.
const SETTLE: Duration = Duration::from_millis(100);

/// Pair a client and server over an in-memory link that delays each direction by
/// [`LATENCY`], awaiting establishment on both ends. Tests run on paused time, so
/// the latency is exact and nothing waits on the wall clock.
async fn pair(client_cfg: Config, server_cfg: Config) -> (Session, Session) {
    let (client_io, client_link) = tokio::io::duplex(64 * 1024);
    let (server_link, server_io) = tokio::io::duplex(64 * 1024);
    let (client_rx, client_tx) = tokio::io::split(client_link);
    let (server_rx, server_tx) = tokio::io::split(server_link);
    tokio::spawn(delay(client_rx, server_tx));
    tokio::spawn(delay(server_rx, client_tx));

    let client = Stream::new(client_io, client_cfg.version, client_cfg.max_record_size);
    let server = Stream::new(server_io, server_cfg.version, server_cfg.max_record_size);
    let (client, server) = tokio::join!(
        Session::connect(client, client_cfg),
        Session::accept(server, server_cfg),
    );
    (client.unwrap(), server.unwrap())
}

/// Copy `from` to `to`, delivering each chunk [`LATENCY`] after it was read.
async fn delay(mut from: impl AsyncRead + Unpin, mut to: impl AsyncWrite + Unpin + Send + 'static) {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let forward = tokio::spawn(async move {
        while let Some((at, chunk)) = rx.recv().await {
            tokio::time::sleep_until(at).await;
            if to.write_all(&chunk).await.is_err() {
                return;
            }
        }
    });

    let mut buf = vec![0u8; 16 * 1024];
    while let Ok(n @ 1..) = from.read(&mut buf).await {
        if tx
            .send((Instant::now() + LATENCY, buf[..n].to_vec()))
            .is_err()
        {
            break;
        }
    }
    drop(tx);
    let _ = forward.await;
}

/// A config that probes every [`INTERVAL`].
fn probing(version: Version) -> Config {
    let mut cfg = Config::new(version);
    cfg.rtt_interval = INTERVAL;
    cfg
}

/// Both ends measure an RTT from the QX_PING exchange, with no application
/// traffic at all. Before RTT tracking, `stats()` reported `None` forever.
#[tokio::test(start_paused = true)]
async fn both_ends_measure_rtt() {
    let (client, server) = pair(probing(Version::QMux02), probing(Version::QMux02)).await;

    tokio::time::sleep(SETTLE).await;

    // The link's round trip exactly, rather than a placeholder standing in for
    // "unknown" or a figure that includes local queueing.
    assert_eq!(client.stats().rtt(), Some(2 * LATENCY), "client");
    assert_eq!(server.stats().rtt(), Some(2 * LATENCY), "server");
}

/// RTT is measured while the application is actively sending, which is exactly
/// when the activity-gated keep-alive ping never fires.
#[tokio::test(start_paused = true)]
async fn measures_rtt_under_load() {
    let (client, _server) = pair(probing(Version::QMux02), probing(Version::QMux02)).await;

    // Keep the client busy so its `last_send_at` never goes quiet.
    let sender = client.clone();
    let busy = tokio::spawn(async move {
        loop {
            let mut send = match sender.open_uni().await {
                Ok(send) => send,
                Err(_) => return,
            };
            if send.write(&[0u8; 512]).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    tokio::time::sleep(SETTLE).await;
    assert!(!busy.is_finished(), "the busy sender stopped early");
    busy.abort();

    // The busy side is the one whose keep-alive never fires.
    assert_eq!(
        client.stats().rtt(),
        Some(2 * LATENCY),
        "no RTT measured while actively sending"
    );
}

/// A zero interval opts out, leaving RTT unreported.
#[tokio::test(start_paused = true)]
async fn zero_interval_disables_probing() {
    let mut cfg = Config::new(Version::QMux02);
    cfg.rtt_interval = Duration::ZERO;
    let (client, _server) = pair(cfg.clone(), cfg).await;

    tokio::time::sleep(SETTLE).await;
    assert_eq!(client.stats().rtt(), None);
}

/// A TCP session reports the kernel's RTT immediately, before any QX_PING could
/// have completed a round trip.
///
/// This is what lets a peer decide at handshake time whether it can report
/// latency at all: an estimate that only appears one probe interval later is
/// indistinguishable, at SETUP, from a transport that never reports one.
#[cfg(unix)]
#[tokio::test]
async fn tcp_reports_rtt_immediately() {
    let mut cfg = Config::new(Version::QMux02);
    // No QX_PING probing at all, so the only possible source is the socket.
    cfg.rtt_interval = Duration::ZERO;

    let (client, server) = pair_with_socket_stats(cfg.clone(), cfg).await;

    for (side, session) in [("client", &client), ("server", &server)] {
        let rtt = session
            .stats()
            .rtt()
            .unwrap_or_else(|| panic!("{side} reported no RTT at handshake"));
        // Loopback. A wildly wrong figure would mean the mirrored `tcp_info` layout
        // has drifted and we are reading some other field.
        assert!(
            rtt < Duration::from_secs(1),
            "{side} implausible RTT: {rtt:?}"
        );
    }
}

/// Like [`pair`], but attaches the kernel's view of each socket the way the
/// `qmux::tcp` builder does.
#[cfg(unix)]
async fn pair_with_socket_stats(client_cfg: Config, server_cfg: Config) -> (Session, Session) {
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};

    fn stats(sock: &TcpStream) -> qmux::SharedSocketStats {
        Arc::new(qmux::TcpStats::new(sock).unwrap())
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let stats = stats(&sock);
        let transport = Stream::new(sock, server_cfg.version, server_cfg.max_record_size)
            .with_socket_stats(stats);
        Session::accept(transport, server_cfg).await.unwrap()
    });

    let sock = TcpStream::connect(addr).await.unwrap();
    let stats = stats(&sock);
    let transport =
        Stream::new(sock, client_cfg.version, client_cfg.max_record_size).with_socket_stats(stats);
    let client = Session::connect(transport, client_cfg).await.unwrap();

    (client, server.await.unwrap())
}
