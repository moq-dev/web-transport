use super::*;
use noq as backend;

fn client() -> Client {
    #[cfg(feature = "aws-lc-rs")]
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    #[cfg(all(feature = "ring", not(feature = "aws-lc-rs")))]
    let _ = rustls::crypto::ring::default_provider().install_default();
    ClientBuilder::new()
        .dangerous()
        .with_no_certificate_verification()
        .unwrap()
}

fn bound(client: &Client) -> Vec<SocketAddr> {
    client
        .endpoints
        .lock()
        .unwrap()
        .binding
        .endpoints()
        .map(|endpoint| endpoint.local_addr().unwrap())
        .collect()
}

fn url(remote: SocketAddr) -> url::Url {
    url::Url::parse(&format!("https://{remote}/")).unwrap()
}

#[test]
fn construction_needs_no_runtime_or_socket() {
    assert!(bound(&client()).is_empty());
}

#[test]
fn ipv6_socket_is_v6_only() {
    let socket = bind_socket("[::]:0".parse().unwrap()).unwrap();
    assert!(socket2::SockRef::from(&socket).only_v6().unwrap());
}

#[tokio::test]
async fn default_ipv4_socket() {
    let client = client();
    let connect = client.connect(url("127.0.0.1:443".parse().unwrap()));
    tokio::pin!(connect);
    assert!(futures::poll!(&mut connect).is_pending());
    assert!(bound(&client)[0].is_ipv4());
    client.close(0u32.into(), b"");
    assert!(connect.await.is_err());
    client.wait_idle().await;
}

#[tokio::test]
async fn close_prevents_binding_either_family() {
    let client = client();
    let clone = client.clone();
    let pending = clone.connect(url("127.0.0.1:443".parse().unwrap()));
    client.close(0u32.into(), b"");
    assert!(pending.await.is_err());
    for host in ["127.0.0.1", "[::1]", "localhost", "[::ffff:127.0.0.1]"] {
        assert!(clone
            .connect(url::Url::parse(&format!("https://{host}:443/")).unwrap())
            .await
            .is_err());
    }
    assert!(bound(&client).is_empty());
    client.wait_idle().await;
}

#[tokio::test]
async fn close_interrupts_connect_and_prevents_second_family() {
    let client = client();
    let clone = client.clone();
    let connect = clone.connect(url("127.0.0.1:443".parse().unwrap()));
    tokio::pin!(connect);
    assert!(futures::poll!(&mut connect).is_pending());
    let before = bound(&client);
    client.close(0u32.into(), b"");
    assert!(connect.await.is_err());
    assert!(client
        .connect(url("[::1]:443".parse().unwrap()))
        .await
        .is_err());
    assert_eq!(bound(&client), before);
    clone.wait_idle().await;
}

#[test]
fn endpoint_failure_is_a_connect_error() {
    // Poll an IP-literal connect without a runtime. Construction succeeds,
    // while the lazy endpoint reports its setup error instead of panicking.
    let client = client();
    let error = futures::executor::block_on(client.connect(url("127.0.0.1:443".parse().unwrap())))
        .unwrap_err();
    assert!(matches!(error, ClientError::IoError(_)));
    assert!(bound(&client).is_empty());
}

#[tokio::test]
async fn caller_endpoint_keeps_its_address_and_remote_handling() {
    let endpoint = backend::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();
    let client = Client::new(endpoint, client().config);
    let error = client
        .connect(url("[::ffff:127.0.0.1]:443".parse().unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::NoqError(backend::ConnectError::InvalidRemoteAddress(_))
    ));
    assert_eq!(bound(&client), vec![addr]);
    client.close(0u32.into(), b"");
    client.wait_idle().await;
}

fn server(addr: SocketAddr) -> crate::Server {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    crate::ServerBuilder::new()
        .with_addr(addr)
        .with_certificate(
            vec![cert.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap()
}

async fn pair(
    client: &Client,
    server: &mut crate::Server,
    remote: SocketAddr,
) -> (Session, Session) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (client, server) = tokio::join!(client.connect(url(remote)), async {
            server.accept().await.unwrap().ok().await.unwrap()
        },);
        (client.unwrap(), server)
    })
    .await
    .expect("loopback connection timed out")
}

#[tokio::test]
async fn families_mapped_addresses_and_clones_share_endpoints() {
    let client = client();
    let clone = client.clone();
    let mut ipv4 = server("127.0.0.1:0".parse().unwrap());
    let mut ipv6 = server("[::1]:0".parse().unwrap());
    let addr4 = ipv4.local_addr().unwrap();
    let addr6 = ipv6.local_addr().unwrap();
    let (session4, peer4) = pair(&client, &mut ipv4, addr4).await;
    let first = bound(&client);
    assert_eq!(first.len(), 1);
    assert!(first[0].is_ipv4());
    let (session6, peer6) = pair(&clone, &mut ipv6, addr6).await;
    let both = bound(&client);
    assert_eq!(both.len(), 2);
    assert!(both[1].is_ipv6());
    assert_eq!(both[0], first[0]);
    let mapped = SocketAddr::new(
        std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(),
        addr4.port(),
    );
    let (session_mapped, peer_mapped) = pair(&clone, &mut ipv4, mapped).await;
    assert!(session_mapped
        .path(noq::PathId::ZERO)
        .unwrap()
        .remote_address()
        .unwrap()
        .is_ipv4());
    assert_eq!(
        peer4
            .path(noq::PathId::ZERO)
            .unwrap()
            .remote_address()
            .unwrap(),
        peer_mapped
            .path(noq::PathId::ZERO)
            .unwrap()
            .remote_address()
            .unwrap()
    );
    assert_eq!(bound(&clone), both);

    client.close(42u32.into(), b"all endpoints");
    clone.close(43u32.into(), b"second close");
    for session in [&session4, &session6, &session_mapped] {
        let error = backend::Connection::closed(session).await;
        assert!(
            matches!(error, backend::ConnectionError::LocallyClosed),
            "{error:?}"
        );
    }
    for peer in [&peer4, &peer6, &peer_mapped] {
        backend::Connection::closed(peer).await;
    }
    tokio::join!(client.wait_idle(), clone.wait_idle());
    assert!(client
        .endpoints
        .lock()
        .unwrap()
        .binding
        .endpoints()
        .all(|endpoint| endpoint.open_connections() == 0));
    drop((session4, session6, session_mapped));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_first_connections_share_one_socket() {
    let client = client();
    let clone = client.clone();
    let mut server = server("127.0.0.1:0".parse().unwrap());
    let remote = server.local_addr().unwrap();
    let (first, second, peers) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            client.connect(url(remote)),
            clone.connect(url(remote)),
            async {
                let first = server.accept().await.unwrap().ok().await.unwrap();
                let second = server.accept().await.unwrap().ok().await.unwrap();
                (first, second)
            }
        )
    })
    .await
    .expect("concurrent loopback connections timed out");
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(bound(&client).len(), 1);
    assert_eq!(
        peers
            .0
            .path(noq::PathId::ZERO)
            .unwrap()
            .remote_address()
            .unwrap(),
        peers
            .1
            .path(noq::PathId::ZERO)
            .unwrap()
            .remote_address()
            .unwrap()
    );
    client.close(0u32.into(), b"");
    client.wait_idle().await;
    drop((first, second, peers));
}

#[tokio::test]
async fn dropping_native_client_keeps_returned_session_usable() {
    let client = client();
    let mut server = server("127.0.0.1:0".parse().unwrap());
    let remote = server.local_addr().unwrap();
    let (session, peer) = pair(&client, &mut server, remote).await;
    drop(client);
    session
        .send_datagram(bytes::Bytes::from_static(b"still open"))
        .unwrap();
    let received = tokio::time::timeout(std::time::Duration::from_secs(10), peer.read_datagram())
        .await
        .expect("datagram after client drop timed out")
        .unwrap();
    assert_eq!(received.as_ref(), b"still open");
    server.close(0u32.into(), b"");
    backend::Connection::closed(&session).await;
    server.wait_idle().await;
}
