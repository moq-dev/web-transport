//! TLS and WSS establish authenticated QMux sessions without a process provider.

#![cfg(any(feature = "tls", feature = "wss"))]

use std::{sync::Arc, time::Duration};

use qmux::{Session, Version};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

fn certificate() -> (CertificateDer<'static>, PrivatePkcs8KeyDer<'static>, String) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    (
        cert.cert.der().clone(),
        PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()),
        cert.cert.pem(),
    )
}

async fn exchange(client: Session, server: Session) {
    assert_eq!(client.protocol(), Some("test"));
    assert_eq!(server.protocol(), Some("test"));
    for (sender, receiver) in [(&client, &server), (&server, &client)] {
        let mut stream = sender.open_uni().await.unwrap();
        stream.write(b"authenticated payload").await.unwrap();
        stream.finish().unwrap();
        let mut stream = receiver.accept_uni().await.unwrap();
        assert_eq!(
            stream.read_all().await.unwrap(),
            b"authenticated payload"[..]
        );
    }
    client.close(0, "done");
    server.close(0, "done");
}

#[cfg(feature = "tls")]
#[tokio::test]
async fn tls_preserves_caller_configs() {
    tokio::time::timeout(Duration::from_secs(10), async {
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
            let (cert, key, _) = certificate();
            let provider = Arc::new(rind_rustls::default_provider().unwrap());
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert.clone()).unwrap();
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots.clone()),
                provider.clone(),
            )
            .build()
            .unwrap();
            let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
                .with_protocol_versions(&[version])
                .unwrap()
                .with_client_cert_verifier(verifier)
                .with_single_cert(vec![cert.clone()], key.clone_key().into())
                .unwrap();
            server.alpn_protocols = vec![b"qmux-02.test".to_vec()];
            let mut client = rustls::ClientConfig::builder_with_provider(provider)
                .with_protocol_versions(&[version])
                .unwrap()
                .with_root_certificates(roots)
                .with_client_auth_cert(vec![cert], key.into())
                .unwrap();
            client.alpn_protocols = vec![b"caller-owned".to_vec()];
            let client = Arc::new(client);
            let server = Arc::new(server);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let acceptor = qmux::tls::Server::new(server.clone());
            let accept = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                acceptor.accept(socket).await.unwrap()
            });
            let session = qmux::tls::Client::new(client.clone())
                .with_protocol("test", &[Version::QMux02])
                .require_protocol()
                .connect(addr, "localhost")
                .await
                .unwrap();
            exchange(session, accept.await.unwrap()).await;
            assert_eq!(client.alpn_protocols, [b"caller-owned".to_vec()]);
            assert_eq!(server.alpn_protocols, [b"qmux-02.test".to_vec()]);
        }
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    })
    .await
    .unwrap();
}

#[cfg(feature = "wss")]
#[test]
fn wss_native_roots_and_caller_config() {
    // Root overrides are process-wide, so each case gets a fresh process.
    const CHILD: &str = "QMUX_WSS_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let (_, key, pem) = certificate();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.tmp")
            .join(format!("qmux-wss-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        std::fs::write(dir.join("root.pem"), pem).unwrap();
        std::fs::write(dir.join("key.der"), key.secret_pkcs8_der()).unwrap();
        std::fs::write(dir.join("empty.pem"), "").unwrap();
        std::fs::write(
            dir.join("malformed.pem"),
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        for case in [
            "default",
            "installed",
            "invalid-provider",
            "untrusted",
            "wrong-name",
            "custom",
            "empty",
            "missing",
            "malformed",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "wss_native_roots_and_caller_config",
                    "--nocapture",
                ])
                .env(CHILD, case)
                .env("QMUX_WSS_TEST_DIR", &dir)
                .env(
                    "SSL_CERT_FILE",
                    dir.join(match case {
                        "custom" | "empty" => "empty.pem",
                        "missing" => "missing.pem",
                        "malformed" => "malformed.pem",
                        _ => "root.pem",
                    }),
                )
                .env("SSL_CERT_DIR", dir.join("empty"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{case}: {output:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), wss_handshake())
                .await
                .unwrap();
        });
}

#[cfg(feature = "wss")]
async fn wss_handshake() {
    use qmux::ws::{self, tokio_tungstenite::Connector};
    use rustls::pki_types::pem::PemObject as _;

    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    let case = std::env::var("QMUX_WSS_TEST_CHILD").unwrap();
    if case == "invalid-provider" {
        let mut provider = rind_rustls::default_provider().unwrap();
        provider.cipher_suites.clear();
        provider.install_default().unwrap();
        let err = ws::Client::new()
            .connect("wss://localhost:1/")
            .await
            .err()
            .unwrap();
        let qmux::Error::Io(err) = err else {
            panic!("{err:?}")
        };
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        return;
    }
    if matches!(case.as_str(), "empty" | "missing" | "malformed") {
        let err = ws::Client::new()
            .connect("wss://localhost:1/")
            .await
            .err()
            .unwrap();
        let qmux::Error::Io(err) = err else {
            panic!("{case}: {err:?}")
        };
        assert_eq!(
            err.kind(),
            if case == "empty" {
                std::io::ErrorKind::NotFound
            } else {
                std::io::ErrorKind::InvalidData
            }
        );
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var_os("QMUX_WSS_TEST_DIR").unwrap());
    let cert = CertificateDer::from_pem_file(dir.join("root.pem")).unwrap();
    let key = PrivatePkcs8KeyDer::from(std::fs::read(dir.join("key.der")).unwrap());
    if case == "installed" {
        let mut provider = rind_rustls::default_provider().unwrap();
        provider
            .cipher_suites
            .retain(|suite| suite.suite() == rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
        provider.install_default().unwrap();
    }
    let provider = Arc::new(rind_rustls::default_provider().unwrap());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap();
    let server = if case == "custom" {
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots.clone()),
            provider.clone(),
        )
        .build()
        .unwrap();
        server.with_client_cert_verifier(verifier)
    } else {
        server.with_no_client_auth()
    };
    let (server_cert, server_key) = if case == "untrusted" {
        let (cert, key, _) = certificate();
        (cert, key)
    } else {
        (cert.clone(), key.clone_key())
    };
    let server = server
        .with_single_cert(vec![server_cert], server_key.into())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let installed = case == "installed";
    let accept = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let socket = tokio_rustls::TlsAcceptor::from(Arc::new(server))
            .accept(socket)
            .await?;
        if installed {
            assert_eq!(
                socket
                    .get_ref()
                    .1
                    .negotiated_cipher_suite()
                    .unwrap()
                    .suite(),
                rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
            );
        }
        ws::Server::new()
            .with_protocol("test", &[Version::QMux02])
            .require_protocol()
            .accept(socket)
            .await
    });
    let mut client = ws::Client::new()
        .with_protocol("test", &[Version::QMux02])
        .require_protocol();
    let mut caller_config = None;
    if case == "custom" {
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![cert], key.into())
            .unwrap();
        config.alpn_protocols = vec![b"caller-owned".to_vec()];
        let config = Arc::new(config);
        client = client.with_connector(Connector::Rustls(config.clone()));
        caller_config = Some(config);
    }
    let host = if case == "wrong-name" {
        "127.0.0.1"
    } else {
        "localhost"
    };
    let result = client
        .connect(&format!("wss://{host}:{}/", addr.port()))
        .await;
    if case == "untrusted" || case == "wrong-name" {
        assert!(result.is_err(), "{case} certificate was accepted");
        assert!(accept.await.unwrap().is_err());
    } else {
        exchange(result.unwrap(), accept.await.unwrap().unwrap()).await;
    }
    if let Some(config) = caller_config {
        assert_eq!(config.alpn_protocols, [b"caller-owned".to_vec()]);
    }
    assert_eq!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        case == "installed"
    );
}
