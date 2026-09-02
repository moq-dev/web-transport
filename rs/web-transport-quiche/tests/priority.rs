//! Per-stream send prioritization.
//!
//! The application sets an `i32` send order where higher is sent first, but quiche
//! schedules by an 8-bit urgency where *lower* is sent first. The connection
//! bridges the two by ranking its send streams, so this drives that ranking over a
//! real connection rather than the unit tests' in-memory bookkeeping: the streams
//! that finish first must be the ones that asked to.

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::sync::mpsc;
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

/// Enough streams that the scheduler has real choices to make, few enough that
/// the whole payload fits inside the default connection flow-control window.
const STREAMS: usize = 32;

/// Big enough that a stream can't be drained in a single congestion window, so
/// the scheduler has to keep choosing between them.
const PAYLOAD: usize = 256 * 1024;

/// How many completions to inspect, and the band they must fall in. Strict
/// urgency ordering makes these exactly the top four; the slack absorbs
/// scheduling noise while staying far out of reach of an unprioritized run.
const CHECKED: usize = 4;
const TOP: usize = 8;

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

/// Each stream carries one repeated byte so the reader can name it without
/// depending on the order the streams arrive in.
fn tag(stream: usize) -> u8 {
    stream as u8
}

/// Accept `STREAMS` unidirectional streams, reading each to EOF and reporting its
/// tag in *completion* order.
async fn spawn_server() -> Result<(SocketAddr, mpsc::UnboundedReceiver<u8>)> {
    let (chain, key) = make_self_signed()?;

    let bind: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let mut server = ServerBuilder::default()
        .with_bind(bind)?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    let (done, finished) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let request = server.accept().await.context("server accept")?;
        let session = request.ok().await.context("server session")?;

        for _ in 0..STREAMS {
            let mut recv = session.accept_uni().await.context("accept uni")?;
            let done = done.clone();

            tokio::spawn(async move {
                let mut tag = None;
                let mut read = 0;
                let mut buf = [0u8; 8 * 1024];

                while let Some(n) = recv.read(&mut buf).await? {
                    if n == 0 {
                        break;
                    }
                    tag.get_or_insert(buf[0]);
                    read += n;
                }

                assert_eq!(read, PAYLOAD, "stream {tag:?} truncated");
                let _ = done.send(tag.context("stream carried no data")?);

                anyhow::Ok(())
            });
        }

        anyhow::Ok(())
    });

    Ok((addr, finished))
}

/// Streams that asked to go first do, even though quiche only has 256 urgencies
/// to spend and the send order is an `i32`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn higher_priority_finishes_first() -> Result<()> {
    init_tracing();

    let (addr, mut finished) = spawn_server().await?;

    let mut settings = Settings::default();
    settings.verify_peer = false;

    let session = ClientBuilder::default()
        .with_settings(settings)
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?)
        .await?
        .established()
        .await
        .context("client handshake")?;

    // Open every stream and queue its payload before any of them can drain, so the
    // scheduler is choosing between all of them rather than servicing whichever
    // happened to be ready. The send orders are spread far apart to make the point
    // that they are ranked, not truncated to a `u8`.
    let mut writers = Vec::with_capacity(STREAMS);
    for stream in 0..STREAMS {
        let mut send = session.open_uni().await.context("open uni")?;
        send.set_priority((STREAMS - stream) as i32 * 1_000_000);

        writers.push(tokio::spawn(async move {
            send.write_all(&vec![tag(stream); PAYLOAD]).await?;
            send.finish()?;
            send.closed().await?;
            anyhow::Ok(())
        }));
    }

    let mut order = Vec::with_capacity(CHECKED);
    for _ in 0..CHECKED {
        order.push(finished.recv().await.context("stream never completed")?);
    }

    for writer in writers {
        writer.await??;
    }

    for tag in &order {
        assert!(
            (*tag as usize) < TOP,
            "stream {tag} finished in the first {CHECKED} despite ranking below the top {TOP}: {order:?}"
        );
    }

    session.close(0, "bye");
    session.closed().await;
    Ok(())
}
