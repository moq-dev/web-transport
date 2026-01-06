use anyhow::Context;

use clap::Parser;
use http::{HeaderValue, StatusCode};
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
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::init_from_env(env);

    let args = Args::parse();

    log::info!("generating self-signed certificate");
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let cert = cert.cert.into();

    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(args.addr)
        .with_certificate(vec![cert], rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;

    log::info!(
        "listening on {}",
        server.local_addr().expect("Couldn't listen")
    );

    // Accept new connections.
    while let Some(conn) = server.accept().await {
        log::debug!("New connection {:?}", conn.url());
        tokio::spawn(async move {
            let err = run_conn(conn).await;
            if let Err(err) = err {
                log::error!("connection failed: {err}")
            }
        });
    }

    Ok(())
}

enum Protocol {
    EchoV0,
    PingV0,
}

impl TryFrom<&HeaderValue> for Protocol {
    type Error = &'static str;

    fn try_from(value: &HeaderValue) -> Result<Self, Self::Error> {
        match value.as_bytes() {
            b"echo/0" => Ok(Protocol::EchoV0),
            b"ping/0" => Ok(Protocol::PingV0),
            _ => Err("Unsupported protocol"),
        }
    }
}

async fn run_conn(request: web_transport_quinn::Request) -> anyhow::Result<()> {
    log::info!("received WebTransport request: {}", request.url());
    let Some(protocol) = request
        .subprotocols()
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
                .ok_with_subprotocol(HeaderValue::from_static("echo/0"))
                .await
                .context("failed to accept session")?;
            log::info!("accepted session");

            // Run the echo session
            if let Err(err) = run_echo_v0(session).await {
                log::info!("closing session: {err}");
            }
        }
        Protocol::PingV0 => {
            // Accept the session.
            let session = request
                .ok_with_subprotocol(HeaderValue::from_static("ping/0"))
                .await
                .context("failed to accept session")?;
            log::info!("accepted session");

            // Run the ping session
            if let Err(err) = run_ping_v0(session).await {
                log::info!("closing session: {err}");
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
                log::info!("accepted stream");

                // Read the message and echo it back.
                let msg = recv.read_to_end(1024).await?;
                log::info!("recv: {}", String::from_utf8_lossy(&msg));

                send.write_all(&msg).await?;
                log::info!("send: {}", String::from_utf8_lossy(&msg));
            },
            res = session.read_datagram() => {
                let msg = res?;
                log::info!("accepted datagram");
                log::info!("recv: {}", String::from_utf8_lossy(&msg));

                session.send_datagram(msg.clone())?;
                log::info!("send: {}", String::from_utf8_lossy(&msg));
            },
        };

        log::info!("echo successful!");
    }
}

async fn run_ping_v0(session: Session) -> anyhow::Result<()> {
    loop {
        // Wait for a bidirectional stream or datagram.
        let (mut send, mut recv) = session.accept_bi().await?;
        log::info!("accepted ping stream");

        // Read the message and echo it back.
        let msg = recv.read_to_end(1024).await?;
        let ping_msg = String::from_utf8_lossy(&msg);
        if ping_msg == "ping" {
            log::info!("ping received");
            send.write_all(b"ack").await?;
        } else {
            log::info!("inccorect command, {}. closing sessions", ping_msg);
            session.close(1, b"unknown command");
        }
    }
}
