//! Integration smoke test for the UniFFI exports.
//!
//! Lives in `src/test.rs` rather than `tests/echo.rs` because cargo refuses
//! to compile integration tests against a crate whose only crate-types are
//! `staticlib` and `cdylib` (moq cfc35faf). The test reuses the same
//! [`RUNTIME`] the runtime exports use.

use std::time::Duration;

use crate::client::{Client, ClientConfig, CongestionControl};
use crate::error::WebTransportError;
use crate::server::{Server, ServerConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

fn self_signed_cert() -> (Vec<Vec<u8>>, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(["localhost".to_string()])
        .expect("generate self-signed cert");
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();
    (vec![cert_der], key_der)
}

fn client_config_for(cert_hash: Vec<u8>) -> ClientConfig {
    ClientConfig {
        server_certificate_hashes: Some(vec![cert_hash]),
        no_cert_verification: false,
        congestion_control: CongestionControl::Default,
        max_idle_timeout_secs: Some(10.0),
        keep_alive_interval_secs: None,
    }
}

#[tokio::test]
async fn echo_datagram() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let (chain, key) = self_signed_cert();
    let cert_hash = sha256(&chain[0]);

    let server = Server::new(ServerConfig {
        certificate_chain: chain,
        private_key: key,
        bind: "127.0.0.1:0".to_string(),
        congestion_control: CongestionControl::Default,
        max_idle_timeout_secs: Some(10.0),
        keep_alive_interval_secs: None,
    })
    .expect("server");

    let addr = server.local_addr();
    let url = format!("https://{}:{}", addr.host, addr.port);

    let server_task = tokio::spawn({
        let server = server.clone();
        async move {
            let req = server.accept().await.expect("accept");
            let session = req.accept().await.expect("ok");
            let dg = session.receive_datagram().await.expect("recv dg");
            session.send_datagram(dg).expect("send echo");
            let _ = session.wait_closed().await;
        }
    });

    let client = Client::new(client_config_for(cert_hash)).expect("client");
    let session = tokio::time::timeout(TIMEOUT, client.connect(url))
        .await
        .expect("connect timeout")
        .expect("connect");

    session.send_datagram(b"hello".to_vec()).expect("send");
    let echoed = tokio::time::timeout(TIMEOUT, session.receive_datagram())
        .await
        .expect("echo timeout")
        .expect("recv");
    assert_eq!(echoed, b"hello");

    session.close(0, String::new());
    let _ = tokio::time::timeout(TIMEOUT, server_task).await;
}

#[test]
fn client_rejects_incompatible_args() {
    let result = Client::new(ClientConfig {
        server_certificate_hashes: Some(vec![vec![0u8; 32]]),
        no_cert_verification: true,
        congestion_control: CongestionControl::Default,
        max_idle_timeout_secs: Some(10.0),
        keep_alive_interval_secs: None,
    });
    let err = match result {
        Ok(_) => panic!("expected invalid argument error"),
        Err(e) => e,
    };
    assert!(matches!(err, WebTransportError::InvalidArgument(_)));
}

fn sha256(cert_der: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hasher.finalize().to_vec()
}

async fn dual_family_sessions(
    client: &Client,
) -> (Vec<ServerSession>, Vec<std::sync::Arc<Server>>) {
    let (chain, key) = self_signed_cert();
    let mut sessions = Vec::new();
    let mut servers = Vec::new();
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let server = Server::new(ServerConfig {
            certificate_chain: chain.clone(),
            private_key: key.clone(),
            bind: bind.into(),
            congestion_control: CongestionControl::Default,
            max_idle_timeout_secs: Some(10.0),
            keep_alive_interval_secs: None,
        })
        .unwrap();
        let addr = server.local_addr();
        let remote = std::net::SocketAddr::new(addr.host.parse().unwrap(), addr.port);
        let (session, peer) = tokio::join!(client.connect(format!("https://{remote}/")), async {
            server.accept().await.unwrap().accept().await.unwrap()
        },);
        sessions.push((session.unwrap(), peer));
        servers.push(server);
    }
    (sessions, servers)
}

type ServerSession = (
    std::sync::Arc<crate::session::Session>,
    std::sync::Arc<crate::session::Session>,
);

#[tokio::test]
async fn client_close_and_drop_cover_both_families() {
    tokio::time::timeout(TIMEOUT, async {
        for explicit_close in [true, false] {
            let client = Client::new(ClientConfig {
                no_cert_verification: true,
                ..Default::default()
            })
            .unwrap();
            let (sessions, servers) = dual_family_sessions(&client).await;
            if explicit_close {
                client.close(42, "all sockets".into()).unwrap();
                assert!(client
                    .connect("https://127.0.0.1:443/".into())
                    .await
                    .is_err());
                assert!(client.connect("https://[::1]:443/".into()).await.is_err());
                client.wait_closed().await;
            }
            drop(client);
            for (session, peer) in &sessions {
                assert!(session.receive_datagram().await.is_err());
                assert!(peer.receive_datagram().await.is_err());
            }
            for server in servers {
                server.close(0, String::new()).unwrap();
                server.wait_closed().await;
            }
        }
    })
    .await
    .expect("client shutdown timed out");
}

#[tokio::test]
async fn closing_unused_client_prevents_first_bind() {
    let client = Client::new(ClientConfig::default()).unwrap();
    client.close(0, String::new()).unwrap();
    client.wait_closed().await;
    for url in [
        "https://127.0.0.1:443/",
        "https://[::1]:443/",
        "https://[::ffff:127.0.0.1]:443/",
    ] {
        assert!(client.connect(url.into()).await.is_err());
    }
}
