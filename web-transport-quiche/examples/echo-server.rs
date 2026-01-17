use std::path;

use anyhow::Context;

use bytes::Bytes;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "[::]:4443")]
    bind: std::net::SocketAddr,

    /// Use the certificates at this path, encoded as PEM.
    #[arg(long)]
    tls_cert: path::PathBuf,

    /// Use the private key at this path, encoded as PEM.
    #[arg(long)]
    tls_key: path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Enable info logging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let tls = web_transport_quiche::ez::CertificatePath {
        cert: args
            .tls_cert
            .to_str()
            .context("failed to convert path to str")?,
        private_key: args
            .tls_key
            .to_str()
            .context("failed to convert path to str")?,
        kind: web_transport_quiche::ez::CertificateKind::X509,
    };

    let mut server = web_transport_quiche::ServerBuilder::default()
        .with_bind(args.bind)?
        .with_cert(tls)?;

    tracing::info!("listening on {}", args.bind);

    // Accept new connections.
    while let Some(conn) = server.accept().await {
        tracing::info!("accepted connection, url={}", conn.url());

        tokio::spawn(async move {
            match run_conn(conn).await {
                Ok(()) => tracing::info!("connection closed"),
                Err(err) => tracing::error!("connection closed: {err}"),
            }
        });
    }

    tracing::info!("server closed");

    Ok(())
}

async fn run_conn(request: web_transport_quiche::h3::Request) -> anyhow::Result<()> {
    tracing::info!("received WebTransport request: {}", request.url());

    // Accept the session.
    let session = request
        .respond(http::StatusCode::OK)
        .await
        .context("failed to accept session")?;
    tracing::info!("accepted session");

    loop {
        let (mut send, mut recv) = session.accept_bi().await?;

        // Wait for a bidirectional stream or datagram (TODO).
        tracing::info!("accepted stream");

        // Read the message and echo it back.
        let mut msg: Bytes = recv.read_all(1024).await?;
        tracing::info!("recv: {}", String::from_utf8_lossy(&msg));

        tracing::info!("send: {}", String::from_utf8_lossy(&msg));
        send.write_buf_all(&mut msg).await?;
        send.finish()?;

        tracing::info!("echo successful!");
    }
}
