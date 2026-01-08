use anyhow::Context;

use clap::Parser;
use http::StatusCode;
use web_transport_quinn::Session;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "[::]:56789")]
    addr: std::net::SocketAddr,
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

    tracing::info!("generating self-signed certificate");
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let cert = cert.cert.into();

    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(args.addr)
        .with_certificate(vec![cert], rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;

    tracing::info!(
        addr = %server.local_addr().expect("Couldn't listen"),
        "listening"
    );

    // Accept new connections.
    while let Some(conn) = server.accept().await {
        tracing::debug!(url = %conn.url(), "New connection");
        tokio::spawn(async move {
            let err = run_conn(conn).await;
            if let Err(err) = err {
                tracing::error!(error = ?err, "connection failed")
            }
        });
    }

    Ok(())
}

enum Protocol {
    EchoV0,
    PingV0,
}

impl TryFrom<&String> for Protocol {
    type Error = ();

    fn try_from(value: &String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "echo/0" => Ok(Protocol::EchoV0),
            "ping/0" => Ok(Protocol::PingV0),
            _ => Err(()),
        }
    }
}

async fn run_conn(request: web_transport_quinn::Request) -> anyhow::Result<()> {
    tracing::info!(url = %request.url(),"received WebTransport request");
    let Some(protocol) = request
        .subprotocols
        .iter()
        .filter_map(|subprotocol| Protocol::try_from(subprotocol).ok())
        .next()
    else {
        // no valid protocol found, we close the connection
        return Ok(request.close(StatusCode::BAD_REQUEST).await?);
    };

    match protocol {
        Protocol::EchoV0 => {
            // Accept the session.
            let session = request
                .ok_with_protocol("echo/0".to_owned())
                .await
                .context("failed to accept session")?;
            tracing::info!("accepted session");

            // Run the echo session
            if let Err(err) = run_echo_v0(session).await {
                tracing::info!(%err, "closing session");
            }
        }
        Protocol::PingV0 => {
            // Accept the session.
            let session = request
                .ok_with_protocol("ping/0".to_owned())
                .await
                .context("failed to accept session")?;
            tracing::info!("accepted session");

            // Run the ping session
            if let Err(err) = run_ping_v0(session).await {
                tracing::info!(%err, "closing session");
            }
        }
    }

    Ok(())
}

async fn run_echo_v0(session: Session) -> anyhow::Result<()> {
    // echo/0 as per the echo-example
    loop {
        // Wait for a bidirectional stream or datagram.
        tokio::select! {
            res = session.accept_bi() => {
                let (mut send, mut recv) = res?;
                tracing::info!("accepted stream");

                // Read the message and echo it back.
                let msg = recv.read_to_end(1024).await?;
                tracing::info!(msg = %String::from_utf8_lossy(&msg), " ⬅️ recv");

                send.write_all(&msg).await?;
                tracing::info!(msg = %String::from_utf8_lossy(&msg), " ➡️ sent");
            },
            res = session.read_datagram() => {
                let msg = res?;
                tracing::info!("accepted datagram");
                tracing::info!(msg = %String::from_utf8_lossy(&msg), " ⬅️ recv");

                session.send_datagram(msg.clone())?;
                tracing::info!(msg = %String::from_utf8_lossy(&msg), " ➡️ sent");
            },
        };

        tracing::info!("echo successful!");
    }
}

async fn run_ping_v0(session: Session) -> anyhow::Result<()> {
    loop {
        // Wait for a bidirectional stream or datagram.
        let (mut send, mut recv) = session.accept_bi().await?;
        tracing::info!("accepted ping stream");

        // Read the message and echo it back.
        let msg = recv.read_to_end(1024).await?;
        let ping_msg = String::from_utf8_lossy(&msg);
        if ping_msg == "ping" {
            tracing::info!("ping received. sending ack");
            send.write_all(b"ack").await?;
        } else {
            tracing::info!(command = %ping_msg, "incorrect command. closing sessions");
            session.close(1, b"unknown command");
            return Ok(());
        }
    }
}
