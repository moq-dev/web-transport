//! Stream and data turnover with small receive windows, so every round trip
//! replenishes MAX_STREAMS or MAX_DATA. Scales independent sessions and the
//! concurrent tasks sharing each session's credit. Run with
//! `cargo bench -p qmux --bench credit`.
use std::time::Instant;

use qmux::{transport::Stream, Config, Session, Version};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

const ROUNDS: usize = 4096;
const MESSAGE: usize = 256;

fn config() -> Config {
    let mut config = Config::new(Version::QMux02);
    config.max_streams_bidi = 16;
    config.max_data = 16 * 1024;
    config
}

async fn pair() -> (Session, Session) {
    let (a, b) = tokio::io::duplex(256 * 1024);
    let cfg = config();
    let server = tokio::spawn({
        let cfg = cfg.clone();
        Session::accept(Stream::new(b, cfg.version, cfg.max_record_size), cfg)
    });
    let client = Session::connect(Stream::new(a, cfg.version, cfg.max_record_size), cfg)
        .await
        .unwrap();
    (client, server.await.unwrap().unwrap())
}

fn serve(server: Session) {
    tokio::spawn(async move {
        while let Ok((mut send, mut recv)) = server.accept_bi().await {
            tokio::spawn(async move {
                let data = recv.read_all().await?;
                send.write_all(&data).await?;
                send.finish()
            });
        }
    });
}

/// Run `ROUNDS` echo round trips per session, split across `tasks` tasks.
async fn run(sessions: usize, tasks: usize) -> f64 {
    let mut pairs = Vec::new();
    for _ in 0..sessions {
        let (client, server) = pair().await;
        serve(server.clone());
        pairs.push((client, server));
    }

    let start = Instant::now();
    let mut handles = Vec::new();
    for (client, _) in &pairs {
        for _ in 0..tasks {
            let client = client.clone();
            handles.push(tokio::spawn(async move {
                let message = vec![7u8; MESSAGE];
                for _ in 0..ROUNDS / tasks {
                    let (mut send, mut recv) = client.open_bi().await.unwrap();
                    send.write_all(&message).await.unwrap();
                    send.finish().unwrap();
                    assert_eq!(recv.read_all().await.unwrap().len(), MESSAGE);
                }
            }));
        }
    }
    for handle in handles {
        handle.await.unwrap();
    }
    let elapsed = start.elapsed();

    for (client, server) in pairs {
        client.close(0, "done");
        server.close(0, "done");
    }
    (sessions * ROUNDS) as f64 / elapsed.as_secs_f64()
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    println!("sessions,tasks_per_session,streams_per_sec");
    for sessions in [1usize, 4, 16] {
        for tasks in [1usize, 16, 64] {
            let rate = runtime.block_on(run(sessions, tasks));
            println!("{sessions},{tasks},{rate:.0}");
        }
    }
}
