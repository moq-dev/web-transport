//! Concurrent accepters must all be woken.
//!
//! Every clone of a `Session` polls the same `SessionAccept`, and it kept no wakers
//! of its own — each poll registered with the inner accept stream, which stores
//! exactly one. A second accepter registered through a waker that replaced the
//! first's, so the first never woke again even as streams arrived.

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
    time::Duration,
};

use anyhow::{Context as _, Result};
use rcgen::{CertifiedKey, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::{io::AsyncReadExt, time::timeout};
use url::Url;
use web_transport_quiche::{ClientBuilder, ServerBuilder, Settings};

/// A waker that records whether it was ever woken.
#[derive(Default)]
struct FlagWaker {
    woken: AtomicBool,
}

impl FlagWaker {
    fn woken(&self) -> bool {
        self.woken.load(Ordering::SeqCst)
    }
}

impl Wake for FlagWaker {
    fn wake(self: Arc<Self>) {
        self.woken.store(true, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::SeqCst);
    }
}

fn make_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .context("rcgen self-signed")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_bytes = KeyPair::serialize_der(&signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes));

    Ok((vec![cert_der], key_der))
}

fn client() -> ClientBuilder {
    let mut settings = Settings::default();
    settings.verify_peer = false;
    ClientBuilder::default().with_settings(settings)
}

/// Register an accepter, let a second one resolve, and assert the first was woken so
/// it can retry. Deterministic — the client is held on a channel until the first
/// accepter is registered, so there is no race to lose and no sleep to tune.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_accept_wakes_every_accepter() -> Result<()> {
    let (chain, key) = make_self_signed()?;

    let mut server = ServerBuilder::default()
        .with_bind::<SocketAddr>((Ipv4Addr::LOCALHOST, 0).into())?
        .with_single_cert(chain, key)?;

    let addr = *server
        .local_addrs()
        .first()
        .context("server has no local address")?;

    // Held until the first accepter has registered its waker.
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    let server_task = tokio::spawn(async move {
        let request = server.accept().await.expect("a connection");
        let session = request.ok().await.expect("an established session");

        // The first accepter registers, then parks. No stream can have arrived yet:
        // the client is still waiting on the channel.
        let flag = Arc::new(FlagWaker::default());
        let waker = Waker::from(flag.clone());
        let mut first = std::pin::pin!(session.accept_uni());
        assert!(
            first
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending(),
            "no stream should be available yet"
        );

        registered_tx.send(()).expect("client is listening");

        // The second accepter registers over the top of the first and resolves.
        let mut recv = session.accept_uni().await.expect("a stream");
        let mut data = Vec::new();
        recv.read_to_end(&mut data).await.expect("the payload");
        assert_eq!(data, b"hi");

        assert!(
            flag.woken(),
            "the first accepter's waker was dropped, so it would never retry"
        );

        done_tx.send(()).expect("client is listening");
    });

    let url = Url::parse(&format!("https://127.0.0.1:{}/", addr.port()))?;
    let session = client()
        .with_bind((Ipv4Addr::LOCALHOST, 0))?
        .connect(url)
        .await?
        .established()
        .await?;

    registered_rx.await?;

    let mut send = session.open_uni().await?;
    send.write_all(b"hi").await?;
    send.finish()?;

    timeout(Duration::from_secs(10), done_rx)
        .await
        .context("the server never confirmed: the first accepter was not woken")??;

    session.close(0, "bye");
    server_task.abort();
    Ok(())
}
