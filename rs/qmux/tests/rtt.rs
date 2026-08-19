//! Round-trip time reported from QX_PING exchanges, over the TCP transport.

#![cfg(feature = "tcp")]

use std::time::Duration;

use qmux::{transport::Stream, Config, Session, Version};
use tokio::net::{TcpListener, TcpStream};
use web_transport_trait::{SendStream as _, Session as _, Stats as _};

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

/// A config that probes often enough to keep the test quick.
fn probing(version: Version) -> Config {
    let mut cfg = Config::new(version);
    cfg.rtt_interval = Duration::from_millis(20);
    cfg
}

/// Poll `session` until it reports an RTT, or give up.
async fn wait_for_rtt(session: &Session) -> Option<Duration> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(rtt) = session.stats().rtt() {
                return rtt;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .ok()
}

/// Both ends measure an RTT from the QX_PING exchange, with no application
/// traffic at all. Before RTT tracking, `stats()` reported `None` forever.
#[tokio::test]
async fn both_ends_measure_rtt() {
    let (client, server) = pair(probing(Version::QMux02), probing(Version::QMux02)).await;

    let client_rtt = wait_for_rtt(&client).await.expect("client measured no RTT");
    let server_rtt = wait_for_rtt(&server).await.expect("server measured no RTT");

    // Loopback, so the figure is small, but it must be a real measurement rather
    // than a zero placeholder standing in for "unknown".
    for rtt in [client_rtt, server_rtt] {
        assert!(rtt < Duration::from_secs(1), "implausible RTT: {rtt:?}");
    }
}

/// RTT is measured while the application is actively sending, which is exactly
/// when the activity-gated keep-alive ping never fires.
#[tokio::test]
async fn measures_rtt_under_load() {
    let (client, server) = pair(probing(Version::QMux02), probing(Version::QMux02)).await;

    // Keep the connection busy so `last_send_at` never goes quiet.
    let busy = tokio::spawn(async move {
        loop {
            let mut send = match client.open_uni().await {
                Ok(send) => send,
                Err(_) => return,
            };
            if send.write(&[0u8; 512]).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    let rtt = wait_for_rtt(&server).await;
    busy.abort();

    assert!(rtt.is_some(), "no RTT measured while actively sending");
}

/// A zero interval opts out, leaving RTT unreported.
#[tokio::test]
async fn zero_interval_disables_probing() {
    let mut cfg = Config::new(Version::QMux02);
    cfg.rtt_interval = Duration::ZERO;
    let (client, _server) = pair(cfg.clone(), cfg).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(client.stats().rtt(), None);
}
