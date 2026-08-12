//! The peer half of the browser harness for `web-transport-wasm`.
//!
//! The echo servers only ever answer; nothing in them opens a stream toward the
//! client or resets one, so a browser client cannot exercise accept, a peer reset,
//! or a session close against them. This one is driven by the client instead: it
//! reads a command off a bidirectional stream and does what it says, which is what
//! lets the harness set up each scenario deterministically rather than by timing.
//!
//! Run it with the certificate from `dev/setup`, then serve the harness page:
//!
//! ```text
//! just harness
//! ```
//!
//! Commands, one per bidirectional stream, as a UTF-8 line:
//!
//! - `echo <text>` — write `<text>` back and finish.
//! - `streams <n>` — open `n` unidirectional streams carrying `stream-0`, `stream-1`,
//!   … then finish. The harness uses this to exercise accept.
//! - `reset <code>` — reset this stream with `<code>`, so the client sees a peer
//!   RESET_STREAM.
//! - `close <code> <reason>` — close the whole session.

use std::{fs, io, path};

use anyhow::Context;
use clap::Parser;
use rustls::pki_types::CertificateDer;
use web_transport_quinn::{proto::ConnectResponse, Session};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "[::]:4443")]
    addr: std::net::SocketAddr,

    /// Use the certificates at this path, encoded as PEM.
    #[arg(long)]
    pub tls_cert: path::PathBuf,

    /// Use the private key at this path, encoded as PEM.
    #[arg(long)]
    pub tls_key: path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let chain = fs::File::open(args.tls_cert).context("failed to open cert file")?;
    let chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut io::BufReader::new(chain))
        .collect::<Result<_, _>>()
        .context("failed to load certs")?;
    anyhow::ensure!(!chain.is_empty(), "could not find certificate");

    let keys = fs::File::open(args.tls_key).context("failed to open key file")?;
    let key = rustls_pemfile::private_key(&mut io::BufReader::new(keys))
        .context("failed to load private key")?
        .context("missing private key")?;

    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(args.addr)
        .with_certificate(chain, key)?;

    tracing::info!(addr = %args.addr, "harness server listening");

    while let Some(conn) = server.accept().await {
        tokio::spawn(async move {
            if let Err(err) = run_conn(conn).await {
                tracing::info!(?err, "connection ended");
            }
        });
    }

    Ok(())
}

async fn run_conn(request: web_transport_quinn::Request) -> anyhow::Result<()> {
    let session = request
        .respond(ConnectResponse::OK)
        .await
        .context("failed to accept session")?;
    tracing::info!("accepted session");

    run_session(session).await
}

async fn run_session(session: Session) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            res = session.accept_bi() => {
                let (send, mut recv) = res?;
                let command = recv.read_to_end(4096).await?;
                let command = String::from_utf8_lossy(&command).trim().to_string();
                tracing::info!(%command, "command");

                let session = session.clone();
                tokio::spawn(async move {
                    if let Err(err) = run_command(session, send, command).await {
                        tracing::warn!(?err, "command failed");
                    }
                });
            },
            // Datagrams are echoed, so the harness can drive send and recv together.
            res = session.read_datagram() => {
                let msg = res?;
                session.send_datagram(msg)?;
            },
        }
    }
}

async fn run_command(
    session: Session,
    mut send: web_transport_quinn::SendStream,
    command: String,
) -> anyhow::Result<()> {
    let (verb, rest) = match command.split_once(' ') {
        Some((verb, rest)) => (verb, rest),
        None => (command.as_str(), ""),
    };

    match verb {
        "echo" => {
            send.write_all(rest.as_bytes()).await?;
            send.finish()?;
        }
        "streams" => {
            let count: usize = rest.trim().parse().context("bad stream count")?;

            // Held until every stream is open, so none is dropped while the harness
            // is still working through the earlier ones.
            let mut streams = Vec::with_capacity(count);
            for i in 0..count {
                let mut stream = session.open_uni().await?;
                stream.write_all(format!("stream-{i}").as_bytes()).await?;
                stream.finish()?;
                streams.push(stream);
            }

            send.finish()?;
        }
        "reset" => {
            let code: u32 = rest.trim().parse().context("bad reset code")?;
            send.reset(code)?;
        }
        "close" => {
            let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            let code: u32 = code.trim().parse().context("bad close code")?;
            session.close(code, reason.as_bytes());
        }
        _ => anyhow::bail!("unknown command: {command}"),
    }

    Ok(())
}
