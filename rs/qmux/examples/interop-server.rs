//! WebSocket server used by the cross-language QMux smoke test.
//!
//! The TypeScript client starts this example once per supported QMux draft. The
//! server accepts only the requested draft, so completing the scenario proves
//! the WebSocket subprotocol negotiation and both implementations' wire formats
//! agree.

use std::io::Write as _;
use std::time::Duration;

use anyhow::{bail, Context as _};
use bytes::Bytes;
use qmux::{Session, Version};
use tokio::net::TcpListener;
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

const PROTOCOL: &str = "qmux-interop";
const PAYLOAD_LEN: usize = 1_200_000;
const CLIENT_SEED: usize = 17;
const SERVER_SEED: usize = 29;
const TIMEOUT: Duration = Duration::from_secs(15);

fn parse_version(value: &str) -> anyhow::Result<Version> {
    match value {
        "qmux-00" => Ok(Version::QMux00),
        "qmux-01" => Ok(Version::QMux01),
        "qmux-02" => Ok(Version::QMux02),
        _ => bail!("unsupported QMux version: {value}"),
    }
}

fn payload(seed: usize) -> Vec<u8> {
    (0..PAYLOAD_LEN)
        .map(|index| ((index * 31 + seed) % 251) as u8)
        .collect()
}

async fn run(session: Session, version: Version) -> anyhow::Result<()> {
    if session.protocol() != Some(PROTOCOL) {
        bail!("unexpected negotiated protocol: {:?}", session.protocol());
    }

    // TypeScript -> Rust, large enough to require both stream- and
    // connection-level flow-control updates with the default Rust windows.
    let mut recv = tokio::time::timeout(TIMEOUT, session.accept_uni())
        .await
        .context("timed out accepting the client unidirectional stream")??;
    let received = tokio::time::timeout(TIMEOUT, recv.read_all())
        .await
        .context("timed out reading the client flow-control payload")??;
    if received.as_ref() != payload(CLIENT_SEED) {
        bail!("client flow-control payload did not match");
    }

    // Rust -> TypeScript. The client advertises deliberately small receive
    // windows, so this cannot finish unless it consumes and replenishes credit.
    let mut send = tokio::time::timeout(TIMEOUT, session.open_uni())
        .await
        .context("timed out opening the server unidirectional stream")??;
    let response = payload(SERVER_SEED);
    tokio::time::timeout(TIMEOUT, send.write_all(&response))
        .await
        .context("timed out writing the server flow-control payload")??;
    send.finish()?;

    // Exercise both halves of one bidirectional stream and FIN propagation.
    let (mut send, mut recv) = tokio::time::timeout(TIMEOUT, session.accept_bi())
        .await
        .context("timed out accepting the bidirectional stream")??;
    let request = tokio::time::timeout(TIMEOUT, recv.read_all())
        .await
        .context("timed out reading the bidirectional request")??;
    let expected = format!("ping:{}", version.alpn());
    if request.as_ref() != expected.as_bytes() {
        bail!("unexpected bidirectional request");
    }
    let response = format!("pong:{}", version.alpn());
    tokio::time::timeout(TIMEOUT, send.write_all(response.as_bytes()))
        .await
        .context("timed out writing the bidirectional response")??;
    send.finish()?;

    // DATAGRAM is available only on record-framed drafts (QMux01+).
    if version.uses_records() {
        session.send_datagram(Bytes::from_static(b"rust-datagram"))?;
        let datagram = tokio::time::timeout(TIMEOUT, session.recv_datagram())
            .await
            .context("timed out receiving the TypeScript datagram")??;
        if datagram.as_ref() != b"typescript-datagram" {
            bail!("unexpected TypeScript datagram");
        }
    } else if session.max_datagram_size() != 0 {
        bail!("QMux00 unexpectedly negotiated datagram support");
    }

    let closed = tokio::time::timeout(TIMEOUT, session.closed())
        .await
        .context("timed out waiting for the TypeScript client to close")?;
    match closed {
        qmux::Error::ConnectionClosed { code, reason }
            if code.into_inner() == 42 && reason == "interop complete" => {}
        other => bail!("unexpected session close: {other}"),
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let raw_version = std::env::args()
        .nth(1)
        .context("usage: interop-server <qmux-00|qmux-01|qmux-02>")?;
    let version = parse_version(&raw_version)?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("ws://{addr}/interop");
    std::io::stdout().flush()?;

    let (socket, _) = listener.accept().await?;
    let session = qmux::ws::Server::new()
        .with_protocol(PROTOCOL, &[version])
        .require_protocol()
        .accept(socket)
        .await?;

    run(session, version).await
}
