use clap::Parser;
use http::{HeaderMap, HeaderValue};
use url::Url;
use web_transport_quinn::protocol_negotation;

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
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::init_from_env(env);

    let args = Args::parse();

    let client = web_transport_quinn::ClientBuilder::new()
        .dangerous()
        .with_no_certificate_verification()?;

    log::info!("connecting to {}", args.url);

    // Connect to the given URL.
    let headers = HeaderMap::from_iter(
        [(
            protocol_negotation::AVAILABLE_NAME,
            HeaderValue::from_str(args.protocol.as_str())?,
        )]
        .into_iter(),
    );
    let session = client.connect(args.url, headers).await?;

    // client confirms the server responded with the expected protocol
    let Some(protocol) = session.protocol() else {
        log::warn!("protocol negotiation failed: server didn't respond with a protocol");
        session.close(42069, b"bye");
        session.closed().await;
        return Ok(());
    };
    if protocol != args.protocol {
        log::warn!(
            "protocol mismatch: expected {}, got {}",
            args.protocol,
            protocol
        );
        session.close(42069, b"bye");
        session.closed().await;
        return Ok(());
    }

    log::info!("connected");

    // Create a bidirectional stream.
    let (mut send, mut recv) = session.open_bi().await?;

    log::info!("created stream");

    // Send a message.
    let msg = args.message;
    send.write_all(msg.as_bytes()).await?;
    log::info!("sent: {msg}");

    // Shut down the send stream.
    send.finish()?;

    // Read back the message.
    let msg = recv.read_to_end(1024).await?;
    log::info!("recv: {}", String::from_utf8_lossy(&msg));

    session.close(42069, b"bye");
    session.closed().await;

    Ok(())
}
