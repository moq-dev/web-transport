//! WebSocket client used by the cross-language QMux smoke test.
//!
//! The mirror of `interop-server`: here the TypeScript side accepts the socket
//! and this example dials it, so the roles that decide the stream-id space are
//! swapped. Completing the scenario proves a `Session.accept` server agrees with
//! an independent implementation about role assignment and the wire format.

use std::time::Duration;

use anyhow::{bail, Context as _};
use bytes::Bytes;
use qmux::{Session, Version};
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

    // Rust -> TypeScript, large enough to require both stream- and
    // connection-level flow-control updates.
    let mut send = tokio::time::timeout(TIMEOUT, session.open_uni())
        .await
        .context("timed out opening the client unidirectional stream")??;
    tokio::time::timeout(TIMEOUT, send.write_all(&payload(CLIENT_SEED)))
        .await
        .context("timed out writing the client flow-control payload")??;
    send.finish()?;

    // TypeScript -> Rust. Server-initiated streams are the ids only a
    // `Session.accept` session may mint; a client-role server would be rejected
    // here rather than accepted.
    let mut recv = tokio::time::timeout(TIMEOUT, session.accept_uni())
        .await
        .context("timed out accepting the server unidirectional stream")??;
    let received = tokio::time::timeout(TIMEOUT, recv.read_all())
        .await
        .context("timed out reading the server flow-control payload")??;
    if received.as_ref() != payload(SERVER_SEED) {
        bail!("server flow-control payload did not match");
    }

    // Exercise both halves of one bidirectional stream and FIN propagation.
    let (mut send, mut recv) = tokio::time::timeout(TIMEOUT, session.open_bi())
        .await
        .context("timed out opening the bidirectional stream")??;
    let request = format!("ping:{}", version.alpn());
    tokio::time::timeout(TIMEOUT, send.write_all(request.as_bytes()))
        .await
        .context("timed out writing the bidirectional request")??;
    send.finish()?;
    let response = tokio::time::timeout(TIMEOUT, recv.read_all())
        .await
        .context("timed out reading the bidirectional response")??;
    if response.as_ref() != format!("pong:{}", version.alpn()).as_bytes() {
        bail!("unexpected bidirectional response");
    }

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
        .context("timed out waiting for the TypeScript server to close")?;
    match closed {
        qmux::Error::ConnectionClosed { code, reason }
            if code.into_inner() == 42 && reason == "interop complete" => {}
        other => bail!("unexpected session close: {other}"),
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .context("usage: interop-client <url> <qmux-00|qmux-01|qmux-02>")?;
    let raw_version = args
        .next()
        .context("usage: interop-client <url> <qmux-00|qmux-01|qmux-02>")?;
    let version = parse_version(&raw_version)?;

    let session = qmux::ws::Client::new()
        .with_protocol(PROTOCOL, &[version])
        .require_protocol()
        .connect(&url)
        .await?;

    run(session, version).await
}
