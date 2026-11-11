//! Exercise the adapter against Quinn's unmodified aws-lc-rs adapter without sockets or sleeps.

use bytes::{Bytes, BytesMut};
use quinn_proto::{
    crypto::{KeyPair, PacketKey, Session},
    transport_parameters::TransportParameters,
    Connection, ConnectionHandle, DatagramEvent, Endpoint, EndpointConfig, Event, Side,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use web_transport_quinn::crypto;

struct Peer {
    endpoint: Endpoint,
    config: Arc<EndpointConfig>,
    connections: HashMap<ConnectionHandle, Connection>,
    addr: SocketAddr,
    retry: bool,
    retries: usize,
}

impl Peer {
    fn new(server: Option<Arc<quinn::ServerConfig>>, port: u16, ours: bool) -> Self {
        let mut config = if ours {
            crypto::endpoint_config().unwrap()
        } else {
            EndpointConfig::default()
        };
        config.cid_generator(|| Box::new(quinn_proto::HashedConnectionIdGenerator::from_key(0)));
        let config = Arc::new(config);
        Self {
            endpoint: Endpoint::new(config.clone(), server, true, None),
            config,
            connections: HashMap::new(),
            addr: ([127, 0, 0, 1], port).into(),
            retry: true,
            retries: 0,
        }
    }

    fn receive(&mut self, now: Instant, from: SocketAddr, packet: BytesMut) -> Vec<BytesMut> {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        match self
            .endpoint
            .handle(now, from, None, None, packet, &mut buf)
        {
            Some(DatagramEvent::NewConnection(incoming)) => {
                if self.retry && !incoming.remote_address_validated() {
                    self.retries += 1;
                    let tx = self.endpoint.retry(incoming, &mut buf).unwrap();
                    out.push(BytesMut::from(&buf[..tx.size]));
                } else {
                    let (handle, connection) =
                        self.endpoint.accept(incoming, now, &mut buf, None).unwrap();
                    self.connections.insert(handle, connection);
                }
            }
            Some(DatagramEvent::ConnectionEvent(handle, event)) => self
                .connections
                .get_mut(&handle)
                .unwrap()
                .handle_event(event),
            Some(DatagramEvent::Response(tx)) => out.push(BytesMut::from(&buf[..tx.size])),
            None => {}
        }
        out
    }

    fn transmit(&mut self, now: Instant) -> Vec<BytesMut> {
        let mut out = Vec::new();
        let mut events = Vec::new();
        for (&handle, conn) in &mut self.connections {
            if conn.poll_timeout().is_some_and(|t| t <= now) {
                conn.handle_timeout(now);
            }
            let mut buf = Vec::new();
            while let Some(tx) = conn.poll_transmit(now, 1, &mut buf) {
                assert!(tx.segment_size.is_none());
                out.push(BytesMut::from(&buf[..tx.size]));
                buf.clear();
            }
            while let Some(event) = conn.poll_endpoint_events() {
                events.push((handle, event));
            }
        }
        for (handle, event) in events {
            if let Some(event) = self.endpoint.handle_event(handle, event) {
                self.connections
                    .get_mut(&handle)
                    .unwrap()
                    .handle_event(event);
            }
        }
        out
    }

    fn conn(&mut self) -> &mut Connection {
        self.connections.values_mut().next().unwrap()
    }
}

struct Pair {
    client: Peer,
    server: Peer,
    now: Instant,
}

impl Pair {
    fn drive(&mut self) {
        let end = self.now + Duration::from_secs(1);
        let mut pending_server = Vec::new();
        for _ in 0..10_000 {
            let mut to_server = std::mem::take(&mut pending_server);
            to_server.extend(self.client.transmit(self.now));
            let mut to_client = self.server.transmit(self.now);
            let traffic = !to_server.is_empty() || !to_client.is_empty();
            for packet in to_server.drain(..) {
                to_client.extend(self.server.receive(self.now, self.client.addr, packet));
            }
            for packet in to_client {
                pending_server.extend(self.client.receive(self.now, self.server.addr, packet));
            }
            if traffic || !pending_server.is_empty() {
                continue;
            }
            let deadline = self
                .client
                .connections
                .values_mut()
                .chain(self.server.connections.values_mut())
                .filter_map(Connection::poll_timeout)
                .min();
            let Some(deadline) = deadline.filter(|t| *t <= end) else {
                self.now = end;
                return;
            };
            self.now = deadline;
        }
        panic!("protocol failed to quiesce");
    }

    fn connect(&mut self, config: quinn::ClientConfig) {
        let (handle, conn) = self
            .client
            .endpoint
            .connect(self.now, config, self.server.addr, "localhost")
            .unwrap();
        self.client.connections.insert(handle, conn);
    }

    fn connected(&mut self) {
        for peer in [&mut self.client, &mut self.server] {
            assert!(matches!(
                peer.conn().poll(),
                Some(Event::HandshakeDataReady)
            ));
            assert!(matches!(peer.conn().poll(), Some(Event::Connected)));
        }
    }

    fn traffic(&mut self, side: Side, message: &'static [u8]) {
        let sender = match side {
            Side::Client => &mut self.client,
            Side::Server => &mut self.server,
        };
        sender
            .conn()
            .datagrams()
            .send(Bytes::from_static(message), false)
            .unwrap();
        self.drive();
        let receiver = match side {
            Side::Client => &mut self.server,
            Side::Server => &mut self.client,
        };
        assert_eq!(receiver.conn().datagrams().recv().unwrap(), message);
    }
}

fn configs(
    ours_server: bool,
    suite: rustls::CipherSuite,
) -> (rustls::ClientConfig, quinn::ServerConfig) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into();
    let mut ours = rind_rustls::default_provider().unwrap();
    let mut upstream = rustls::crypto::aws_lc_rs::default_provider();
    for provider in [&mut ours, &mut upstream] {
        provider.cipher_suites.sort_by_key(|s| s.suite() != suite);
    }
    let ours = Arc::new(ours);
    let upstream = Arc::new(upstream);
    let (client_provider, server_provider) = if ours_server {
        (upstream, ours)
    } else {
        (ours, upstream)
    };
    let mut tls = rustls::ServerConfig::builder_with_provider(server_provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key)
        .unwrap();
    tls.alpn_protocols = vec![b"a".to_vec(), b"b".to_vec()];
    tls.max_early_data_size = u32::MAX;
    tls.send_tls13_tickets = 1;
    let server = if ours_server {
        crypto::server_config(Arc::new(crypto::QuicServerConfig::try_from(tls).unwrap())).unwrap()
    } else {
        quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap(),
        ))
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certified.cert.der().clone()).unwrap();
    let mut client = rustls::ClientConfig::builder_with_provider(client_provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"a".to_vec()];
    client.enable_early_data = true;
    (client, server)
}

fn client_crypto(tls: rustls::ClientConfig, ours: bool) -> Arc<dyn quinn::crypto::ClientConfig> {
    if ours {
        Arc::new(crypto::QuicClientConfig::try_from(tls).unwrap())
    } else {
        Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap())
    }
}

fn client_config(tls: rustls::ClientConfig, ours: bool) -> quinn::ClientConfig {
    quinn::ClientConfig::new(client_crypto(tls, ours))
}

#[test]
fn upstream_handshake_retry_and_reset() {
    for suite in [
        rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    ] {
        for ours_server in [false, true] {
            let (tls, server) = configs(ours_server, suite);
            let mut pair = Pair {
                client: Peer::new(None, 4434, !ours_server),
                server: Peer::new(Some(Arc::new(server)), 4433, ours_server),
                now: Instant::now(),
            };
            pair.connect(client_config(tls, !ours_server));
            pair.drive();
            pair.connected();
            assert!(pair.server.retries > 0);
            assert!(!pair.client.conn().is_handshaking());
            assert!(!pair.server.conn().is_handshaking());
            let mut client_export = [0; 32];
            let mut server_export = [0; 32];
            pair.client
                .conn()
                .crypto_session()
                .export_keying_material(&mut client_export, b"test", b"context")
                .unwrap();
            pair.server
                .conn()
                .crypto_session()
                .export_keying_material(&mut server_export, b"test", b"context")
                .unwrap();
            assert_eq!(client_export, server_export);
            pair.traffic(Side::Client, b"from client");
            pair.traffic(Side::Server, b"from server");
            // Restart the rind endpoint and require the unmodified peer to recognize its reset.
            let (restarted, remote) = if ours_server {
                (&mut pair.server, &mut pair.client)
            } else {
                (&mut pair.client, &mut pair.server)
            };
            restarted.endpoint = Endpoint::new(restarted.config.clone(), None, true, None);
            restarted.connections.clear();
            while remote.conn().poll().is_some() {}
            remote.conn().ping();
            pair.drive();
            let remote = if ours_server {
                &mut pair.client
            } else {
                &mut pair.server
            };
            assert!(matches!(
                remote.conn().poll(),
                Some(Event::ConnectionLost {
                    reason: quinn::ConnectionError::Reset
                })
            ));
        }
    }
}

/// Runs a TLS handshake between two sessions and returns each side's 1-RTT packet keys.
fn handshake(
    client: &mut dyn Session,
    server: &mut dyn Session,
) -> [KeyPair<Box<dyn PacketKey>>; 2] {
    // Moves one flight from `from` to `to`, recording keys `from` switched to.
    fn flight(
        from: &mut dyn Session,
        to: &mut dyn Session,
        keys: &mut Option<KeyPair<Box<dyn PacketKey>>>,
    ) -> bool {
        let mut buf = Vec::new();
        let next = from.write_handshake(&mut buf);
        let progress = next.is_some() || !buf.is_empty();
        if let Some(next) = next {
            *keys = Some(next.packet);
        }
        if !buf.is_empty() {
            to.read_handshake(&buf).unwrap();
        }
        progress
    }

    let (mut client_keys, mut server_keys) = (None, None);
    for _ in 0..16 {
        let progress = flight(client, server, &mut client_keys);
        if !(flight(server, client, &mut server_keys) || progress) {
            assert!(!client.is_handshaking() && !server.is_handshaking());
            return [client_keys.unwrap(), server_keys.unwrap()];
        }
    }
    panic!("handshake failed to quiesce");
}

const HEADER: &[u8] = b"header";

fn seal(key: &dyn PacketKey, packet: u64) -> Vec<u8> {
    let mut buf = [HEADER, b"payload", &[0; 16]].concat();
    key.encrypt(packet, &mut buf, HEADER.len());
    buf
}

fn opens(key: &dyn PacketKey, packet: u64, sealed: &[u8]) -> bool {
    let mut payload = BytesMut::from(&sealed[HEADER.len()..]);
    key.decrypt(packet, HEADER, &mut payload).is_ok() && &payload[..] == b"payload"
}

// Quinn's `force_key_update` can start an update before the peer has seen a packet in the current
// key phase (RFC 9001 section 6.1) when it follows Quinn's randomly timed routine key update, so
// a connection that forces updates is flaky. Check key updates on the sessions directly instead.
#[test]
fn upstream_key_updates() {
    let params = TransportParameters::read(Side::Client, &mut std::io::Cursor::new([])).unwrap();
    for suite in [
        rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    ] {
        for ours_server in [false, true] {
            let (tls, server) = configs(ours_server, suite);
            let mut client = client_crypto(tls, !ours_server)
                .start_session(1, "localhost", &params)
                .unwrap();
            let mut server = server.crypto.start_session(1, &params);
            let mut keys = handshake(&mut *client, &mut *server);
            for packet in 0..4 {
                let sealed = keys.each_ref().map(|k| seal(&*k.local, packet));
                assert!(opens(&*keys[1].remote, packet, &sealed[0]));
                assert!(opens(&*keys[0].remote, packet, &sealed[1]));
                keys = [
                    client.next_1rtt_keys().unwrap(),
                    server.next_1rtt_keys().unwrap(),
                ];
                assert!(!opens(&*keys[1].remote, packet, &sealed[0]));
                assert!(!opens(&*keys[0].remote, packet, &sealed[1]));
            }
        }
    }
}

#[test]
fn upstream_zero_rtt_rejection() {
    for ours_server in [false, true] {
        let (mut tls, server) = configs(ours_server, rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
        let mut pair = Pair {
            client: Peer::new(None, 4434, !ours_server),
            server: Peer::new(Some(Arc::new(server)), 4433, ours_server),
            now: Instant::now(),
        };
        pair.server.retry = false;
        pair.connect(client_config(tls.clone(), !ours_server));
        pair.drive();
        pair.connected();
        assert!(!pair.client.conn().has_0rtt());
        let now = pair.now;
        pair.client.conn().close(now, 0u32.into(), Bytes::new());
        pair.drive();
        pair.client.connections.clear();
        pair.server.connections.clear();
        tls.alpn_protocols = vec![b"b".to_vec()];
        pair.connect(client_config(tls, !ours_server));
        assert!(pair.client.conn().has_0rtt());
        pair.client
            .conn()
            .datagrams()
            .send(Bytes::from_static(b"early"), false)
            .unwrap();
        pair.drive();
        pair.connected();
        assert!(!pair.client.conn().accepted_0rtt());
        assert!(pair.server.conn().datagrams().recv().is_none());
        pair.traffic(Side::Client, b"after rejection");
    }
}

#[test]
fn invalid_quic_config_is_rejected() {
    let (tls, _) = configs(true, rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider
        .cipher_suites
        .retain(|s| s.suite() == rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    assert!(matches!(
        crypto::QuicClientConfig::try_from(client),
        Err(crypto::ConfigError::NoInitialCipherSuite)
    ));
    let client = rustls::ClientConfig::builder_with_provider(tls.crypto_provider().clone())
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    assert!(matches!(
        crypto::QuicClientConfig::try_from(client),
        Err(crypto::ConfigError::Tls(_))
    ));
}

#[test]
fn upstream_retry_authentication_rejects_tampering() {
    for ours_server in [false, true] {
        let (tls, server) = configs(ours_server, rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
        let cfg = client_crypto(tls, !ours_server);
        for version in [1, 0xff00_001d] {
            let session = cfg
                .clone()
                .start_session(
                    version,
                    "localhost",
                    &TransportParameters::read(Side::Client, &mut std::io::Cursor::new([]))
                        .unwrap(),
                )
                .unwrap();
            let cid = quinn::ConnectionId::new(b"original");
            let header = b"header";
            let token = b"token";
            let packet = [header.as_slice(), token.as_slice()].concat();
            let tag = server.crypto.retry_tag(version, &cid, &packet);
            let mut payload = [token.as_slice(), tag.as_slice()].concat();
            assert!(session.is_valid_retry(&cid, header, &payload));
            assert!(!session.is_valid_retry(&quinn::ConnectionId::new(b"other"), header, &payload));
            assert!(!session.is_valid_retry(&cid, b"changed", &payload));
            payload[0] ^= 1;
            assert!(!session.is_valid_retry(&cid, header, &payload));
            payload[0] ^= 1;
            *payload.last_mut().unwrap() ^= 1;
            assert!(!session.is_valid_retry(&cid, header, &payload));
            assert!(!session.is_valid_retry(&cid, header, &payload[..15]));
        }
    }
}
