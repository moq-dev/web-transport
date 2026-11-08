//! Rind endpoint keys and TLS interoperate with an independently configured ring peer.

use std::{net::Ipv4Addr, sync::Arc, time::Duration};

use noq::crypto::{HandshakeTokenKey, HmacKey};
use rustls::{pki_types::PrivatePkcs8KeyDer, CipherSuite};
use web_transport_noq::{crypto, noq};

fn endpoint(server: Option<noq::ServerConfig>, ours: bool) -> noq::Endpoint {
    noq::Endpoint::new(
        if ours {
            crypto::endpoint_config().unwrap()
        } else {
            noq::EndpointConfig::default()
        },
        server,
        std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        Arc::new(noq::TokioRuntime),
    )
    .unwrap()
}

async fn handshake(ours_server: bool, suite: CipherSuite, retry: bool) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let mut ours = rind_rustls::default_provider().unwrap();
    let mut ring = rustls::crypto::ring::default_provider();
    // Excluding AES-128 from traffic also exercises the separate initial suite.
    for provider in [&mut ours, &mut ring] {
        provider.cipher_suites.retain(|s| s.suite() == suite);
    }
    let (client_provider, server_provider) = if ours_server {
        (ring, ours)
    } else {
        (ours, ring)
    };
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(server_provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let server_crypto = if ours_server {
        noq::crypto::rustls::QuicServerConfig::with_initial(
            Arc::new(tls),
            crypto::initial_suite().unwrap(),
        )
        .unwrap()
    } else {
        noq::crypto::rustls::QuicServerConfig::with_initial(
            Arc::new(tls),
            rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256
                .tls13()
                .unwrap()
                .quic_suite()
                .unwrap(),
        )
        .unwrap()
    };
    let server_config = if ours_server {
        crypto::server_config(Arc::new(server_crypto)).unwrap()
    } else {
        noq::ServerConfig::with_crypto(Arc::new(server_crypto))
    };
    let server = endpoint(Some(server_config), ours_server);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(client_provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let initial = if ours_server {
        rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256
            .tls13()
            .unwrap()
            .quic_suite()
            .unwrap()
    } else {
        crypto::initial_suite().unwrap()
    };
    let client_config = noq::ClientConfig::new(Arc::new(
        noq::crypto::rustls::QuicClientConfig::with_initial(Arc::new(tls), initial).unwrap(),
    ));
    let client = endpoint(None, !ours_server);
    let connect = client
        .connect_with(client_config, server.local_addr().unwrap(), "localhost")
        .unwrap();
    let accept = async {
        let mut incoming = server.accept().await.unwrap();
        if retry {
            assert!(!incoming.remote_address_validated());
            incoming.retry().unwrap();
            incoming = server.accept().await.unwrap();
            assert!(incoming.remote_address_validated());
        }
        incoming.await.unwrap()
    };
    let (client_conn, server_conn) = tokio::join!(async { connect.await.unwrap() }, accept);
    let mut exported = [[0; 32]; 2];
    for (conn, output) in [&client_conn, &server_conn].into_iter().zip(&mut exported) {
        conn.export_keying_material(output, b"test", b"context")
            .unwrap();
        let data = conn
            .handshake_data()
            .unwrap()
            .downcast::<noq::crypto::rustls::HandshakeData>()
            .unwrap();
        assert_eq!(data.protocol.as_deref(), Some(b"h3".as_slice()));
    }
    assert_eq!(exported[0], exported[1]);
    for (sender, receiver) in [(&client_conn, &server_conn), (&server_conn, &client_conn)] {
        let mut send = sender.open_uni().await.unwrap();
        send.write_all(b"rind and ring").await.unwrap();
        send.finish().unwrap();
        assert_eq!(
            receiver
                .accept_uni()
                .await
                .unwrap()
                .read_to_end(64)
                .await
                .unwrap(),
            b"rind and ring"
        );
    }
    client.close(0u32.into(), b"done");
    server.close(0u32.into(), b"done");
    tokio::join!(client.wait_idle(), server.wait_idle());
}

#[tokio::test]
async fn ring_peer_handshake_and_retry() {
    for suite in [
        CipherSuite::TLS13_AES_128_GCM_SHA256,
        CipherSuite::TLS13_AES_256_GCM_SHA384,
        CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    ] {
        for ours_server in [false, true] {
            for retry in [false, true] {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    handshake(ours_server, suite, retry),
                )
                .await
                .expect("loopback handshake and transfer");
            }
        }
    }
}

#[test]
fn reset_hmac_matches_ring_and_rejects_invalid_signatures() {
    let secret = [42; 64];
    let key = crypto::ResetKey::new(&secret).unwrap();
    let ring_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &secret);
    let mut signature = vec![0; key.signature_len()];
    key.sign(b"connection ID", &mut signature);
    assert_eq!(
        signature,
        ring::hmac::sign(&ring_key, b"connection ID").as_ref()
    );
    key.verify(b"connection ID", &signature).unwrap();
    assert!(key.verify(b"other ID", &signature).is_err());
    assert!(key.verify(b"connection ID", &signature[..16]).is_err());
    signature[0] ^= 1;
    assert!(key.verify(b"connection ID", &signature).is_err());
}

#[test]
fn token_encryption_matches_noq_ring_construction_and_authenticates() {
    let secret = [42; 64];
    let key = crypto::TokenKey::new(&secret).unwrap();
    let nonce = 0x102030405060708090a0b0c0d0e0f000u128;
    // Match noq-proto 1.4's HKDF-SHA256 and little-endian 128-bit nonce.
    let master = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &[]).extract(&secret);
    let nonce_bytes = nonce.to_le_bytes();
    let info = [&nonce_bytes[..]];
    let mut derived = [0; 32];
    master
        .expand(&info, ring::hkdf::HKDF_SHA256)
        .unwrap()
        .fill(&mut derived)
        .unwrap();
    let ring_key = ring::aead::LessSafeKey::new(
        ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, &derived).unwrap(),
    );
    let mut expected = b"address and connection ID".to_vec();
    ring_key
        .seal_in_place_append_tag(
            ring::aead::Nonce::assume_unique_for_key([0; 12]),
            ring::aead::Aad::empty(),
            &mut expected,
        )
        .unwrap();
    let mut sealed = b"address and connection ID".to_vec();
    key.seal(nonce, &mut sealed).unwrap();
    assert_eq!(sealed, expected);
    assert_eq!(
        key.open(nonce, &mut sealed.clone()).unwrap(),
        b"address and connection ID"
    );
    assert!(key.open(nonce ^ 1, &mut sealed.clone()).is_err());
    assert!(crypto::TokenKey::new(&[43; 64])
        .unwrap()
        .open(nonce, &mut sealed.clone())
        .is_err());
    for index in [0, sealed.len() - 1] {
        let mut modified = sealed.clone();
        modified[index] ^= 1;
        assert!(key.open(nonce, &mut modified).is_err());
    }
    for length in [0, 15, sealed.len() - 1] {
        assert!(key.open(nonce, &mut sealed[..length].to_vec()).is_err());
    }
}

#[test]
fn rind_packet_keys_support_noq_path_api() {
    for suite in [
        rind_rustls::cipher_suite::TLS13_AES_128_GCM_SHA256,
        rind_rustls::cipher_suite::TLS13_AES_256_GCM_SHA384,
        rind_rustls::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
    ] {
        let suite = suite.tls13().unwrap().quic_suite().unwrap();
        let keys = suite.keys(
            b"connection ID",
            rustls::Side::Client,
            rustls::quic::Version::V1,
        );
        // Noq uses these methods even for the initial path, whose ID is zero.
        for path in [0, 1, u32::MAX] {
            let mut payload = b"packet payload".to_vec();
            let tag = keys
                .local
                .packet
                .encrypt_in_place_for_path(path, 7, b"header", &mut payload)
                .expect("Noq requires path-aware packet encryption");
            payload.extend_from_slice(tag.as_ref());
            assert_eq!(
                keys.local
                    .packet
                    .decrypt_in_place_for_path(path, 7, b"header", &mut payload)
                    .expect("Noq requires path-aware packet decryption"),
                b"packet payload"
            );
        }
    }
}
