//! Sustained stream and data turnover over TCP with small receive windows, so
//! concurrent consumers replenish MAX_STREAMS and MAX_DATA constantly. Reordered
//! credit updates used to close such sessions with a flow-control error.

#![cfg(feature = "tcp")]

use std::time::Duration;

use qmux::{transport::Stream, Config, Session, Version};
use tokio::net::{TcpListener, TcpStream};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

const TASKS: usize = 32;
const ROUNDS: usize = 100;
const MESSAGE: usize = 256;

fn config(version: Version) -> Config {
    let mut config = Config::new(version);
    config.max_streams_bidi = 8;
    config.max_streams_uni = 8;
    config.max_data = 4096;
    config.max_stream_data_bidi_local = 1024;
    config.max_stream_data_bidi_remote = 1024;
    config.max_stream_data_uni = 1024;
    config
}

async fn pair(version: Version) -> (Session, Session) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = config(version);

    let server = tokio::spawn({
        let cfg = cfg.clone();
        async move {
            let (sock, _) = listener.accept().await.unwrap();
            let transport = Stream::new(sock, cfg.version, cfg.max_record_size);
            Session::accept(transport, cfg).await.unwrap()
        }
    });

    let sock = TcpStream::connect(addr).await.unwrap();
    let transport = Stream::new(sock, cfg.version, cfg.max_record_size);
    let client = Session::connect(transport, cfg).await.unwrap();
    (client, server.await.unwrap())
}

/// Echo every bidi stream and drain every uni stream until the session closes.
fn serve(server: Session) {
    tokio::spawn({
        let server = server.clone();
        async move {
            while let Ok((mut send, mut recv)) = server.accept_bi().await {
                tokio::spawn(async move {
                    let data = recv.read_all().await?;
                    send.write_all(&data).await?;
                    send.finish()
                });
            }
        }
    });
    tokio::spawn(async move {
        while let Ok(mut recv) = server.accept_uni().await {
            tokio::spawn(async move { recv.read_all().await });
        }
    });
}

async fn churn(version: Version) {
    let (client, server) = pair(version).await;
    serve(server.clone());

    let tasks: Vec<_> = (0..TASKS)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move {
                let message = vec![7u8; MESSAGE];
                for _ in 0..ROUNDS {
                    let (mut send, mut recv) = client.open_bi().await?;
                    send.write_all(&message).await?;
                    send.finish()?;
                    let echoed = recv.read_all().await?;
                    assert_eq!(echoed.len(), MESSAGE);

                    let mut uni = client.open_uni().await?;
                    uni.write_all(&message).await?;
                    uni.finish()?;
                }
                Ok::<_, qmux::Error>(())
            })
        })
        .collect();

    // Real networking on a multi-threaded runtime: the deadline only guards
    // against a hang, since a credit-ordering failure closes the session.
    tokio::time::timeout(Duration::from_secs(60), async {
        for task in tasks {
            task.await
                .unwrap()
                .unwrap_or_else(|err| panic!("{version:?}: {err}"));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{version:?}: stream churn stalled"));

    client.close(0, "done");
    server.close(0, "done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_churn_qmux00() {
    churn(Version::QMux00).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_churn_qmux01() {
    churn(Version::QMux01).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_churn_qmux02() {
    churn(Version::QMux02).await;
}
