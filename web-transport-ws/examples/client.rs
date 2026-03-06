use web_transport_trait::{RecvStream, SendStream, Session as _};
use web_transport_ws::Client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = "ws://127.0.0.1:3000";
    println!("Connecting to {url}");

    let client = Client::new();
    let session = client.connect(url).await?;
    println!("WebSocket connection established");

    println!("\n=== Testing unidirectional stream ===");
    let mut uni_stream = session.open_uni().await?;
    uni_stream
        .write(b"Hello from unidirectional stream!")
        .await?;
    uni_stream.finish()?;
    println!("Sent message on unidirectional stream");

    // Receive back the same message
    let mut recv = session.accept_uni().await?;
    let data = recv.read_all().await?;
    println!("Received: {}", String::from_utf8_lossy(&data));

    println!("\n=== Testing bidirectional stream ===");
    let (mut send, mut recv) = session.open_bi().await?;

    let message = b"Hello from bidirectional stream!";
    send.write(message).await?;
    println!("Sent: Hello from bidirectional stream!");

    let response = recv.read_all().await?;
    let text = String::from_utf8_lossy(&response);
    println!("Received: {text}");

    send.finish()?;

    println!("\nClient shutting down...");
    Ok(())
}
