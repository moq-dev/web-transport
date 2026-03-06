use tokio::net::TcpListener;
use web_transport_trait::{RecvStream, SendStream, Session as _};
use web_transport_ws::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:3000";
    let listener = TcpListener::bind(addr).await?;
    println!("WebSocket server listening on ws://{addr}");

    let server = Server::new();

    while let Ok((stream, addr)) = listener.accept().await {
        let server = server.clone();
        tokio::spawn(async move {
            println!("New connection from: {addr}");
            if let Err(e) = run(&server, stream).await {
                println!("Connection error: {e}");
            } else {
                println!("Connection ended");
            }
        });
    }

    Ok(())
}

async fn run(server: &Server, stream: tokio::net::TcpStream) -> anyhow::Result<()> {
    let session = server.accept(stream).await?;
    println!("WebSocket connection established");

    loop {
        tokio::select! {
            Ok(mut uni) = session.accept_uni() => {
                println!("Accepted unidirectional stream");

                // Make a unidirectional stream to echo back the received messages
                let mut echo = session.open_uni().await?;

                println!("Created unidirectional stream to echo back");

                // NOTE: You should spawn a task to read in parallel
                let data = uni.read_all().await?;
                println!(
                    "Received {} bytes on unidirectional stream: {}",
                    data.len(),
                    String::from_utf8_lossy(&data)
                );

                println!("Echoing back {} bytes on unidirectional stream: {}", data.len(), String::from_utf8_lossy(&data));
                echo.write_all(&data).await?;

                echo.finish()?;

                println!("Unidirectional stream closed");
            }
            Ok((mut send, mut recv)) = session.accept_bi() => {
                println!("Accepted bidirectional stream");

                // NOTE: You should spawn a task to read in parallel
                let data = recv.read_all().await?;
                println!("Received {} bytes on bidirectional stream", data.len());

                send.write_all(&data).await?;
                send.finish()?;
                println!("Echoing back {} bytes on bidirectional stream: {}", data.len(), String::from_utf8_lossy(&data));

                println!("Bidirectional stream closed");
            }
            err = session.closed() => {
                return Err(err.into());
            }
        }
    }
}
