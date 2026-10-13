//! Stream-count flow control through a builder's session config.

#![cfg(feature = "ws")]

use std::time::Duration;

use qmux::ws::tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};
use qmux::{ws, Config};
use web_transport_trait::{SendStream as _, Session as _};

/// How many (bidi, uni) stream pairs a session pair holds open at once before
/// opening stalls on stream credit, trying up to `target`. Every stream stays
/// open, so none returns credit.
async fn held(config: Config, target: usize) -> usize {
    const ALPN: &str = "qmux-01.test";
    let (a, b) = tokio::io::duplex(64 * 1024);
    let a = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
    let b = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
    let client = ws::Upgraded::new(a)
        .with_alpn(ALPN)
        .with_session_config(config.clone())
        .connect();
    let server = ws::Upgraded::new(b)
        .with_alpn(ALPN)
        .with_session_config(config)
        .accept();

    let mut opened = Vec::new();
    let mut accepted = Vec::new();
    // Paused time only advances once every task is idle, so this fires only
    // when opening stalls on credit.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        let open = async {
            for _ in 0..target {
                let (mut bi, bi_recv) = client.open_bi().await.unwrap();
                bi.write(b"x").await.unwrap();
                let mut uni = client.open_uni().await.unwrap();
                uni.write(b"x").await.unwrap();
                opened.push((bi, bi_recv, uni));
            }
        };
        let accept = async {
            for _ in 0..target {
                let bi = server.accept_bi().await.unwrap();
                let uni = server.accept_uni().await.unwrap();
                accepted.push((bi, uni));
            }
        };
        tokio::join!(open, accept);
    })
    .await;

    client.close(0, "done");
    server.close(0, "done");
    accepted.len()
}

/// Applications such as MoQ hold a stream per subscription, so they need to
/// grant more than the default.
#[tokio::test(start_paused = true)]
async fn configured_limit_holds_a_thousand_streams() {
    let mut config = Config::default();
    config.max_streams_bidi = 1_000;
    config.max_streams_uni = 1_000;
    assert_eq!(held(config, 1_000).await, 1_000);
}

#[tokio::test(start_paused = true)]
async fn default_limit_stalls_at_a_hundred_streams() {
    assert_eq!(held(Config::default(), 1_000).await, 100);
}
