#![cfg(not(target_arch = "wasm32"))]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use web_transport::{quinn as native, Client};

use native::{crypto, proto::ConnectResponse, quinn};

fn endpoint(server: Option<quinn::ServerConfig>, addr: SocketAddr) -> Result<quinn::Endpoint> {
    Ok(quinn::Endpoint::new(
        crypto::endpoint_config()?,
        server,
        std::net::UdpSocket::bind(addr)?,
        quinn::default_runtime().context("no async runtime")?,
    )?)
}

struct Endpoints {
    server: native::Server,
    endpoint: quinn::Endpoint,
    config: quinn::ClientConfig,
}

impl Endpoints {
    fn new() -> Result<Self> {
        let key = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
        let cert = CertificateDer::from(key.cert.der().to_vec());
        let provider = crypto::default_provider()?;
        let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.clone()],
                PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()).into(),
            )?;
        tls.alpn_protocols = vec![native::ALPN.as_bytes().to_vec()];
        let config = crypto::server_config(Arc::new(crypto::QuicServerConfig::try_from(tls)?))?;
        let addr: SocketAddr = "127.0.0.1:0".parse()?;
        let server = native::Server::new(endpoint(Some(config), addr)?);

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert)?;
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![native::ALPN.as_bytes().to_vec()];
        let mut config =
            quinn::ClientConfig::new(Arc::new(crypto::QuicClientConfig::try_from(tls)?));
        let mut transport = quinn::TransportConfig::default();
        transport.datagram_receive_buffer_size(None);
        config.transport_config(Arc::new(transport));

        Ok(Self {
            server,
            endpoint: endpoint(None, addr)?,
            config,
        })
    }

    fn client(&self) -> Client {
        native::Client::new(self.endpoint.clone(), self.config.clone()).into()
    }

    async fn close(self) {
        self.endpoint.close(0u32.into(), b"done");
        self.server.close(0u32.into(), b"done");
        tokio::join!(self.endpoint.wait_idle(), self.server.wait_idle());
    }
}

#[tokio::test]
async fn conversion_preserves_endpoint_tls_and_transport() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut endpoints = Endpoints::new()?;
        let client = endpoints.client();
        let url = format!("https://{}/room", endpoints.server.local_addr()?).parse()?;
        let accept = async {
            let request = endpoints.server.accept().await.context("no request")?;
            assert_eq!(
                request.conn().remote_address(),
                endpoints.endpoint.local_addr()?
            );
            assert!(request.protocols.is_empty());
            anyhow::Ok(request.ok().await?)
        };
        // The self-signed certificate is trusted only by the supplied config.
        let (session, peer) = tokio::join!(client.connect(url), accept);
        let session = session?;
        let peer = peer?;
        assert_eq!(session.protocol(), None);
        assert_eq!(session.url().unwrap().path(), "/room");
        assert_eq!(peer.max_datagram_size(), 0);

        let mut send = session.open_uni().await?;
        assert_eq!(send.write(b"hello").await?, 5);
        send.finish()?;
        let mut recv = peer.accept_uni().await?;
        assert_eq!(recv.read_to_end(5).await?, b"hello");

        endpoints.close().await;
        anyhow::Ok(())
    })
    .await?
}

async fn negotiate(
    server: &mut native::Server,
    client: &Client,
    offered: &[&str],
    selected: Option<&str>,
) -> Result<()> {
    let url = format!("https://{}/", server.local_addr()?).parse()?;
    let accept = async {
        let request = server.accept().await.context("no request")?;
        assert_eq!(request.protocols, offered);
        let response = match selected {
            Some(protocol) => ConnectResponse::default().with_protocol(protocol),
            None => ConnectResponse::default(),
        };
        anyhow::Ok(request.respond(response).await?)
    };
    let (session, peer) = tokio::join!(client.connect(url), accept);
    let session = session?;
    let peer = peer?;
    assert_eq!(session.protocol(), selected);
    assert_eq!(peer.protocol(), selected);
    session.close(0, "done");
    tokio::join!(session.closed(), peer.closed());
    Ok(())
}

#[tokio::test]
async fn converted_clients_compose_and_clone_protocol_offers() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut endpoints = Endpoints::new()?;
        let client = endpoints.client();
        negotiate(&mut endpoints.server, &client, &[], None).await?;

        let client = client.with_protocols(["first", "second"]);
        let changed = client
            .clone()
            .with_protocols(["discarded"])
            .with_protocols(vec![String::from("replacement")]);
        negotiate(
            &mut endpoints.server,
            &changed,
            &["replacement"],
            Some("replacement"),
        )
        .await?;
        negotiate(
            &mut endpoints.server,
            &client.clone(),
            &["first", "second"],
            Some("second"),
        )
        .await?;
        negotiate(&mut endpoints.server, &client, &["first", "second"], None).await?;
        let cleared = client.with_protocols(std::iter::empty::<&str>());
        negotiate(&mut endpoints.server, &cleared, &[], None).await?;

        endpoints.close().await;
        anyhow::Ok(())
    })
    .await?
}

#[tokio::test]
async fn converted_default_client_binds_per_family_endpoint() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut endpoints = Endpoints::new()?;
        // The native client binds its IPv4 endpoint only when it first dials.
        let client = Client::from(native::Client::from(endpoints.config.clone()))
            .with_protocols(["default"]);
        let url = format!("https://{}/", endpoints.server.local_addr()?).parse()?;
        let accept = async {
            let request = endpoints.server.accept().await.context("no request")?;
            let remote = request.conn().remote_address();
            assert_eq!(remote.ip(), std::net::Ipv4Addr::LOCALHOST);
            assert_ne!(remote, endpoints.endpoint.local_addr()?);
            assert_eq!(request.protocols, ["default"]);
            anyhow::Ok(request.ok().await?)
        };
        let (session, peer) = tokio::join!(client.connect(url), accept);
        let session = session?;
        let peer = peer?;
        assert_eq!(peer.max_datagram_size(), 0);
        session.close(0, "done");
        tokio::join!(session.closed(), peer.closed());

        endpoints.close().await;
        anyhow::Ok(())
    })
    .await?
}
