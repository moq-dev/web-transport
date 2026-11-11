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

#[tokio::test]
async fn default_verification_rejects_untrusted_server() {
    let cert = rcgen::generate_simple_self_signed(["127.0.0.1".to_string()])
        .expect("generate self-signed cert");
    let server = Server::new(ServerConfig {
        certificate_chain: vec![cert.cert.der().to_vec()],
        private_key: cert.signing_key.serialize_der(),
        bind: "127.0.0.1:0".to_string(),
        congestion_control: CongestionControl::Default,
        max_idle_timeout_secs: Some(10.0),
        keep_alive_interval_secs: None,
    })
    .expect("server");
    let addr = server.local_addr();
    let server_task = tokio::spawn({
        let server = server.clone();
        async move {
            let _ = server.accept().await;
        }
    });

    let client = Client::new(ClientConfig::default()).expect("client");
    let err = match tokio::time::timeout(
        TIMEOUT,
        client.connect(format!("https://{}:{}", addr.host, addr.port)),
    )
    .await
    .expect("connect timeout")
    {
        Ok(_) => panic!("connected to a server the OS does not trust"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, WebTransportError::Connect(msg) if msg.contains("certificate")),
        "{err:?}"
    );

    server.close(0, String::new()).expect("close server");
    server_task.abort();
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

    use web_transport_quinn::crypto;

    let (chain, key) = self_signed_cert();
    let cert = rustls::pki_types::CertificateDer::from(chain[0].clone());
    let key = rustls::pki_types::PrivateKeyDer::try_from(key).unwrap();
    let provider = crypto::default_provider().unwrap();
    let endpoint = |server_config| {
        quinn::Endpoint::new(
            crypto::endpoint_config().unwrap(),
            server_config,
            std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap()
    };

    let tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();
    let mut config =
        crypto::server_config(Arc::new(crypto::QuicServerConfig::try_from(tls).unwrap())).unwrap();
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(None);
    transport.max_concurrent_bidi_streams(0u32.into());
    transport.max_concurrent_uni_streams(0u32.into());
    config.transport_config(Arc::new(transport));
    let server = endpoint(Some(config));

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut config =
        quinn::ClientConfig::new(Arc::new(crypto::QuicClientConfig::try_from(tls).unwrap()));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(None);
    config.transport_config(Arc::new(transport));
    let mut client = endpoint(None);
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
