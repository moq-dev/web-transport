use clap::Parser;
use url::Url;

use web_transport_quinn::proto::ConnectRequest;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "https://localhost:56789")]
    url: Url,

    /// WebTransport Subprotocol to use, try 'echo/0' or 'ping/0'
    #[arg(long, default_value = "echo/0")]
    protocol: String,

    /// Message to send to the server. In echo/0 this should be echoed back; for ping/0 this must be `ping` and the server will respond with `ack`.
    #[arg(default_value = "Hello World")]
    message: String,
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

    let client = web_transport_quinn::ClientBuilder::new()
        .dangerous()
        .with_no_certificate_verification()?;

    tracing::info!(url = %args.url, "connecting");

    let session = client
        .connect(ConnectRequest {
            url: args.url,
            protocols: vec![args.protocol.clone()],
        })
        .await?;

    // client confirms the server responded with the expected protocol
    let Some(protocol) = &session.response().protocol else {
        tracing::warn!("protocol negotiation failed: server didn't respond with a protocol");
        session.close(42069, b"bye");
        session.closed().await;
        return Ok(());
    };
    if protocol != &args.protocol {
        tracing::warn!(expected=%args.protocol, found=%protocol, "protocol mismatch. closing.");
        session.close(42069, b"bye");
        session.closed().await;
        return Ok(());
    }

    tracing::info!("connected");

    // Create a bidirectional stream.
    let (mut send, mut recv) = session.open_bi().await?;

    tracing::info!("created stream");

    // Send a message.
    let msg = args.message;
    send.write_all(msg.as_bytes()).await?;
    tracing::info!(%msg, " ➡️ sent");

    // Shut down the send stream.
    send.finish()?;

    // Read back the message.
    let msg = recv.read_to_end(1024).await?;
    tracing::info!(msg = %String::from_utf8_lossy(&msg), " ⬅️ recv",);

    session.close(42069, b"bye");
    session.closed().await;

    Ok(())
}
