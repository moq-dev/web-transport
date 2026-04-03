//! HTTP/3 over QMux (h3qx-01) interop example.
//!
//! Demonstrates running HTTP/3 framing (from `web-transport-proto`) over QMux streams,
//! using the `h3qx-01` ALPN. This is the same wire format as HTTP/3 over QUIC, but
//! the QUIC transport layer is replaced by QMux over TCP.
//!
//! Flow:
//!   1. Client and server establish a QMux session over TCP
//!   2. Each side opens a unidirectional control stream and sends SETTINGS
//!   3. Client opens a bidirectional request stream and sends a CONNECT request
//!   4. Server reads the request and sends back a 200 OK response
//!
//! Run: cargo run -p qmux --example h3qx

use bytes::BytesMut;
use tokio::net::TcpListener;

use qmux::{Session, Version};
use url::Url;
use web_transport_proto::{ConnectRequest, ConnectResponse, Settings};
use web_transport_trait::{RecvStream, SendStream, Session as _};

/// The ALPN for HTTP/3 over QMux draft-01.
const H3QX_ALPN: &str = "h3qx-01";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Bind a TCP listener on an ephemeral port.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("listening on {addr}");

    // Spawn the server task.
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let session = qmux::tcp::accept(stream, Some(Version::QMux01))
            .await
            .unwrap();
        run_server(session).await.unwrap();
    });

    // Connect the client.
    let session = qmux::tcp::connect(addr, Some(Version::QMux01)).await?;
    run_client(session).await?;

    server.await?;
    Ok(())
}

/// Send HTTP/3 SETTINGS on a new unidirectional control stream.
async fn send_settings(session: &Session) -> anyhow::Result<()> {
    let mut uni = session.open_uni().await?;

    let mut buf = BytesMut::new();
    let mut settings = Settings::default();
    settings.enable_webtransport(1);
    settings.encode(&mut buf);

    uni.write(&buf).await?;
    uni.finish()?;

    println!("  sent SETTINGS (enable_webtransport=1)");
    Ok(())
}

/// Receive HTTP/3 SETTINGS from the peer's unidirectional control stream.
async fn recv_settings(session: &Session) -> anyhow::Result<Settings> {
    let mut uni = session.accept_uni().await?;

    // Read all data from the control stream.
    let data = uni.read_all().await?;
    let settings = Settings::decode(&mut data.as_ref())?;

    let wt = settings.supports_webtransport();
    println!("  received SETTINGS (supports_webtransport={wt})");
    Ok(settings)
}

async fn run_client(session: Session) -> anyhow::Result<()> {
    println!("[client] connected");

    // Exchange SETTINGS (both directions concurrently).
    let (_, settings) = tokio::try_join!(send_settings(&session), recv_settings(&session),)?;

    let wt = settings.supports_webtransport();
    assert!(wt > 0, "server does not support WebTransport");

    // Open a bidi request stream and send a CONNECT request.
    let (mut send, mut recv) = session.open_bi().await?;

    let url: Url = "https://localhost/webtransport".parse()?;
    let request = ConnectRequest::new(url).with_protocol(H3QX_ALPN);

    let mut buf = BytesMut::new();
    request.encode(&mut buf)?;
    send.write(&buf).await?;
    send.finish()?;
    println!("[client] sent CONNECT request");

    // Read the response.
    let response_data = recv.read_all().await?;
    let response = ConnectResponse::decode(&mut response_data.as_ref())?;
    println!(
        "[client] received response: {} (protocol={:?})",
        response.status, response.protocol
    );

    assert_eq!(response.status, 200);

    session.close(0, "done");
    println!("[client] closed");
    Ok(())
}

async fn run_server(session: Session) -> anyhow::Result<()> {
    println!("[server] accepted connection");

    // Exchange SETTINGS (both directions concurrently).
    tokio::try_join!(send_settings(&session), recv_settings(&session),)?;

    // Accept the bidi request stream with the CONNECT request.
    let (mut send, mut recv) = session.accept_bi().await?;

    let request_data = recv.read_all().await?;
    let request = ConnectRequest::decode(&mut request_data.as_ref())?;
    println!(
        "[server] received CONNECT request: {} (protocols={:?})",
        request.url, request.protocols
    );

    // Respond with 200 OK.
    let response = ConnectResponse::OK.with_protocol(H3QX_ALPN);

    let mut buf = BytesMut::new();
    response.encode(&mut buf)?;
    send.write(&buf).await?;
    send.finish()?;
    println!("[server] sent 200 OK");

    // Wait for close.
    let err = session.closed().await;
    println!("[server] session closed: {err}");
    Ok(())
}
