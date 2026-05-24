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
