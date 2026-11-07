//! Loopback peer for the Node lifecycle tests, able to fail the underlying QUIC connection.

use std::{io::Read, ops::Deref};

use anyhow::Context;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let session = if let Some(url) = std::env::args().nth(1) {
        let client = web_transport_quinn::ClientBuilder::new()
            .dangerous()
            .with_no_certificate_verification()
            .context("create client")?;
        client
            .connect(url::Url::parse(&url).context("parse peer URL")?)
            .await
            .context("connect to peer")?
    } else {
        let pem = std::env::var("WEB_TRANSPORT_TEST_PEM").context("read test certificate")?;
        let pem = pem.as_bytes();
        let certs = rustls_pemfile::certs(&mut &pem[..])
            .collect::<Result<Vec<_>, _>>()
            .context("parse certificate")?;
        let key = rustls_pemfile::private_key(&mut &pem[..])
            .context("parse private key")?
            .context("missing key")?;
        let mut server = web_transport_quinn::ServerBuilder::new()
            .with_addr("127.0.0.1:0".parse()?)
            .with_certificate(certs, key)
            .context("bind server")?;
        println!("https://{}", server.local_addr().context("server address")?);
        server
            .accept()
            .await
            .context("server closed")?
            .ok()
            .await
            .context("accept session")?
    };

    // The JS side sends a datagram after attaching its closed handler, so the failure
    // always occurs on an established Session rather than racing its handshake.
    session
        .read_datagram()
        .await
        .context("read trigger datagram")?;
    // H3_NO_ERROR is orderly shutdown; raw QUIC 0 is outside WebTransport's HTTP/3 space.
    let code = if std::env::var_os("WEB_TRANSPORT_TEST_ORDERLY_CLOSE").is_some() {
        0x100u32
    } else {
        0
    };
    session.deref().close(code.into(), b"connection shutdown");
    // Await a second signal on stdin to keep the runtime alive while QUIC sends the close.
    tokio::task::spawn_blocking(|| std::io::stdin().read(&mut [0]))
        .await
        .context("join shutdown signal")?
        .context("read shutdown signal")?;
    Ok(())
}
