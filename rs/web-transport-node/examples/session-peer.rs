//! Loopback peer for the Node lifecycle tests, able to fail the underlying QUIC connection.

use std::{io::Read, ops::Deref};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = if let Some(url) = std::env::args().nth(1) {
        let client = web_transport_quinn::ClientBuilder::new()
            .dangerous()
            .with_no_certificate_verification()?;
        client.connect(url::Url::parse(&url)?).await?
    } else {
        let pem = std::env::var("WEB_TRANSPORT_TEST_PEM")?;
        let pem = pem.as_bytes();
        let certs = rustls_pemfile::certs(&mut &pem[..]).collect::<Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut &pem[..])?.ok_or("missing key")?;
        let mut server = web_transport_quinn::ServerBuilder::new()
            .with_addr("127.0.0.1:0".parse()?)
            .with_certificate(certs, key)?;
        println!("https://{}", server.local_addr()?);
        server.accept().await.ok_or("server closed")?.ok().await?
    };

    // The JS side sends a datagram after attaching its closed handler, so the failure
    // always occurs on an established Session rather than racing its handshake.
    session.read_datagram().await?;
    // QUIC application code 0 is outside WebTransport's HTTP/3 code space.
    session.deref().close(0u32.into(), b"connection failure");
    // Await a second signal on stdin to keep the runtime alive while QUIC sends the close.
    tokio::task::spawn_blocking(|| std::io::stdin().read(&mut [0])).await??;
    Ok(())
}
