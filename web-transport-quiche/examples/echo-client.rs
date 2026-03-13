use bytes::Bytes;
use clap::Parser;
use url::Url;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "https://localhost:4443")]
    url: Url,

    /// Dangerous: Disable TLS certificate verification.
    #[arg(long, default_value = "false")]
    tls_disable_verify: bool,
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

    let client = web_transport_quiche::ClientBuilder::default();
    let mut settings = web_transport_quiche::Settings::default();
    settings.verify_peer = !args.tls_disable_verify;

    tracing::info!("connecting to {}", args.url);
    let session = client
        .with_settings(settings)
        .connect(args.url)
        .await?
        .established()
        .await?;

    tracing::info!("connected");

    // Create a bidirectional stream.
    let (mut send, mut recv) = session.open_bi().await?;

    tracing::info!("created stream");

    // Send a message.
    let msg = Bytes::from("hello world");
    tracing::info!("sent: {}", String::from_utf8_lossy(&msg));
    send.write_all(&msg).await?;

    // Shut down the send stream.
    send.finish()?;

    // Read back the message.
    let msg = recv.read_all(1024).await?;
    tracing::info!("recv: {}", String::from_utf8_lossy(&msg));

    session.close(42069, "bye");
    session.closed().await;

    tracing::info!("closed session");

    Ok(())
}
