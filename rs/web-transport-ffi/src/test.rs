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

#[tokio::test]
async fn cancelled_session_calls_release_the_final_owner() {
    use std::{future::Future, task::Poll};

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for operation in 0..4 {
        let (chain, key) = self_signed_cert();
        let cert_hash = sha256(&chain[0]);
        let server = Server::new(ServerConfig {
            certificate_chain: chain,
            private_key: key,
            bind: "127.0.0.1:0".to_string(),
            congestion_control: CongestionControl::Default,
            max_idle_timeout_secs: None,
            keep_alive_interval_secs: None,
        })
        .unwrap();
        let addr = server.local_addr();
        let mut config = client_config_for(cert_hash);
        config.max_idle_timeout_secs = None;
        let client = Client::new(config).unwrap();
        let accepted = async { server.accept().await.unwrap().accept().await.unwrap() };
        let (session, peer) = tokio::join!(
            client.connect(format!("https://{}:{}", addr.host, addr.port)),
            accepted
        );
        let session = session.unwrap();
        let mut call = Box::pin(async {
            match operation {
                0 => {
                    let _ = session.accept_uni().await;
                }
                1 => {
                    let _ = session.accept_bi().await;
                }
                2 => {
                    let _ = session.receive_datagram().await;
                }
                3 => session.wait_closed().await,
                _ => unreachable!(),
            }
        });
        // One poll reaches the RUNTIME spawn, while the peer supplies no stream,
        // datagram or close that could let its owning task complete naturally.
        std::future::poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(call);
        drop(session);
        tokio::time::timeout(TIMEOUT, peer.wait_closed())
            .await
            .expect("cancelled foreign call retained the final Session owner");
    }
}

#[tokio::test]
async fn cancelled_stream_opens_release_the_final_owner() {
    use std::{future::Future, sync::Arc, task::Poll};

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (chain, key) = self_signed_cert();
    let cert = rustls::pki_types::CertificateDer::from(chain[0].clone());
    let key = rustls::pki_types::PrivateKeyDer::try_from(key).unwrap();
    let mut config = quinn::ServerConfig::with_single_cert(vec![cert.clone()], key).unwrap();
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(None);
    transport.max_concurrent_bidi_streams(0u32.into());
    transport.max_concurrent_uni_streams(0u32.into());
    config.transport_config(Arc::new(transport));
    let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let mut config = quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(None);
    config.transport_config(Arc::new(transport));
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(config);

    for bidirectional in [false, true] {
        let connecting = client
            .connect(server.local_addr().unwrap(), "localhost")
            .unwrap();
        let accepted = async { server.accept().await.unwrap().await.unwrap() };
        let (conn, peer) = tokio::join!(connecting, accepted);
        // Raw mode lets the peer grant zero credit without preventing HTTP/3 setup.
        let session =
            crate::session::Session::new(web_transport_quinn::Session::raw(conn.unwrap()));
        let mut call = Box::pin(async {
            if bidirectional {
                let _ = session.open_bi().await;
            } else {
                let _ = session.open_uni().await;
            }
        });
        std::future::poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(call);
        drop(session);
        tokio::time::timeout(TIMEOUT, peer.closed())
            .await
            .expect("cancelled stream open retained the final Session owner");
    }
}
