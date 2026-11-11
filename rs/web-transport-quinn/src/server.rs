use std::sync::Arc;
use std::task::Poll;

use futures::{
    future::{BoxFuture, Either},
    stream::FuturesUnordered,
    StreamExt,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::client::{controller_factory, transport_config, ControllerFactory};
use crate::{crypto, CongestionControl};
use crate::{
    proto::{ConnectRequest, ConnectResponse},
    Connecting, ServerError, Session, Settings,
};

/// Construct a WebTransport [Server] using sane defaults.
///
/// This is optional; advanced users may use [Server::new] directly.
pub struct ServerBuilder {
    provider: Result<crypto::Provider, crypto::UnsupportedCpu>,
    addr: std::net::SocketAddr,
    transport: quinn::TransportConfig,
    congestion_controller: Option<ControllerFactory>,
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerBuilder {
    /// Create a server builder with sane defaults.
    pub fn new() -> Self {
        Self {
            provider: crypto::default_provider(),
            addr: "[::]:443".parse().unwrap(),
            transport: quinn::TransportConfig::default(),
            congestion_controller: None,
        }
    }

    /// Listen on the specified address.
    pub fn with_addr(self, addr: std::net::SocketAddr) -> Self {
        Self { addr, ..self }
    }

    /// Use the supplied QUIC transport configuration instead of Quinn's defaults.
    ///
    /// An explicit [`Self::with_congestion_control`] choice overrides only its
    /// congestion controller, regardless of the order these methods are called.
    pub fn with_transport_config(mut self, transport: quinn::TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    /// Override the transport configuration's congestion controller.
    ///
    /// [`CongestionControl::Default`] preserves the supplied configuration's
    /// controller, or Quinn's default when no configuration is supplied.
    pub fn with_congestion_control(mut self, algorithm: CongestionControl) -> Self {
        self.congestion_controller = controller_factory(algorithm);
        self
    }

    /// Supply a certificate used for TLS.
    // TODO support multiple certs based on...?
    pub fn with_certificate(
        self,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Server, ServerError> {
        let addr = self.addr;
        let config = self.config(chain, key)?;

        let server = quinn::Endpoint::new(
            crypto::endpoint_config()?,
            Some(config),
            std::net::UdpSocket::bind(addr).map_err(|e| ServerError::IoError(e.into()))?,
            quinn::default_runtime().ok_or_else(|| {
                ServerError::IoError(std::io::Error::other("no async runtime found").into())
            })?,
        )
        .map_err(|e| ServerError::IoError(e.into()))?;

        Ok(Server::new(server))
    }

    fn config(
        self,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<quinn::ServerConfig, ServerError> {
        // Standard Quinn setup
        let mut config = rustls::ServerConfig::builder_with_provider(self.provider.clone()?)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(chain, key)?;

        config.alpn_protocols = vec![crate::ALPN.as_bytes().to_vec()]; // this one is important

        let config: crate::crypto::QuicServerConfig = config.try_into()?;
        let mut config = crypto::server_config(Arc::new(config))?;
        config.transport_config(transport_config(
            self.transport,
            self.congestion_controller.as_ref(),
        ));

        Ok(config)
    }
}

/// A WebTransport server that accepts new sessions.
pub struct Server {
    endpoint: quinn::Endpoint,
    accept: FuturesUnordered<BoxFuture<'static, Result<Request, ServerError>>>,
}

impl core::ops::Deref for Server {
    type Target = quinn::Endpoint;

    fn deref(&self) -> &Self::Target {
        &self.endpoint
    }
}

impl Server {
    /// Manually create a new server with a manually constructed Endpoint.
    ///
    /// NOTE: The ALPN must be set to `crate::ALPN` for WebTransport to work.
    pub fn new(endpoint: quinn::Endpoint) -> Self {
        Self {
            endpoint,
            accept: Default::default(),
        }
    }

    /// Accept a new WebTransport session Request from a client.
    pub async fn accept(&mut self) -> Option<Request> {
        loop {
            // Borrow both halves inside the block so the winner can be handled afterwards.
            let winner = {
                let incoming = self.endpoint.accept();
                let pending = &mut self.accept;

                // An empty set of pending handshakes must not end the accept loop.
                let handshake = futures::future::poll_fn(|cx| match pending.poll_next_unpin(cx) {
                    Poll::Ready(Some(res)) => Poll::Ready(res),
                    _ => Poll::Pending,
                });

                futures::pin_mut!(incoming, handshake);

                // Handshakes go first: there are only as many as the arrivals already taken,
                // whereas a steady stream of arrivals would never let them be polled.
                match futures::future::select(handshake, incoming).await {
                    Either::Left((res, _)) => Either::Right(res),
                    Either::Right((incoming, _)) => Either::Left(incoming),
                }
            };

            match winner {
                Either::Left(incoming) => {
                    let conn = incoming?;
                    self.accept.push(Box::pin(async move {
                        let conn = conn.await?;
                        Request::accept(conn).await
                    }));
                }
                Either::Right(res) => {
                    if let Ok(session) = res {
                        return Some(session);
                    }
                }
            }
        }
    }
}

/// A mostly complete WebTransport handshake, just awaiting the server's decision on whether to accept or reject the session based on the URL.
pub struct Request {
    conn: quinn::Connection,
    settings: Settings,
    connect: Connecting,
}

impl Request {
    /// Accept a new WebTransport session from a client.
    pub async fn accept(conn: quinn::Connection) -> Result<Self, ServerError> {
        // Perform the H3 handshake by sending/reciving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;

        // Accept the CONNECT request but don't send a response yet.
        let connect = Connecting::accept(&conn).await?;

        // Return the resulting request with a reference to the settings/connect streams.
        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    pub async fn ok(self) -> Result<Session, ServerError> {
        self.respond(ConnectResponse::default()).await
    }

    /// Reply to the session with the given response, usually 200 OK.
    ///
    /// [ConnectResponse::with_protocol] can be used to select a subprotocol.
    pub async fn respond(
        self,
        response: impl Into<ConnectResponse>,
    ) -> Result<Session, ServerError> {
        let response = response.into();
        let connect = self.connect.respond(response).await?;
        Ok(Session::new(self.conn, self.settings, connect))
    }

    /// Reject the session with a status code or a response containing headers.
    ///
    /// Waits for delivery or peer termination; cancel to impose an application deadline.
    pub async fn reject(self, response: impl Into<ConnectResponse>) -> Result<(), ServerError> {
        self.connect.reject(response).await?;
        Ok(())
    }

    /// Returns the underlying QUIC connection.
    pub fn conn(&self) -> &quinn::Connection {
        &self.conn
    }

    /// The remote peer's address.
    #[deprecated(note = "use conn().remote_address() instead")]
    pub fn remote_address(&self) -> std::net::SocketAddr {
        self.conn.remote_address()
    }

    /// Returns the CONNECT request that was sent by the client.
    ///
    /// DEPRECATED: You can access this via the Deref impl.
    pub fn connect(&self) -> &ConnectRequest {
        &self.connect
    }
}

impl core::ops::Deref for Request {
    type Target = ConnectRequest;

    fn deref(&self) -> &Self::Target {
        &self.connect
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{assert_transport, custom_transport};
    use rustls::pki_types::PrivatePkcs8KeyDer;

    fn self_signed() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let chain = vec![CertificateDer::from(key.cert.der().to_vec())];
        let der = rcgen::KeyPair::serialize_der(&key.signing_key);

        (chain, PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der)))
    }

    pub(crate) fn builder() -> ServerBuilder {
        ServerBuilder::new().with_addr("[::]:0".parse().unwrap())
    }

    /// quinn installs its own default transport config, so the builder's has to be
    /// attached explicitly or every server knob silently no-ops.
    #[test]
    fn builder_transport_reaches_the_config() {
        let (chain, key) = self_signed();

        let config = builder().config(chain, key).unwrap();
        assert_transport(&config.transport, false);

        for transport_first in [false, true] {
            let (chain, key) = self_signed();
            let (transport, _) = custom_transport();
            let builder = if transport_first {
                builder()
                    .with_transport_config(transport)
                    .with_congestion_control(CongestionControl::LowLatency)
            } else {
                builder()
                    .with_congestion_control(CongestionControl::LowLatency)
                    .with_transport_config(transport)
            };
            let config = builder.config(chain, key).unwrap();
            assert_transport(&config.transport, true);
        }
    }

    #[tokio::test]
    async fn builder_transport_reaches_the_server_endpoint() {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        tokio::time::timeout(Duration::from_secs(10), async {
            for transport_first in [false, true] {
                for algorithm in [
                    CongestionControl::Default,
                    CongestionControl::LowLatency,
                    CongestionControl::Throughput,
                ] {
                    let (chain, key) = self_signed();
                    let (transport, built) = custom_transport();
                    let expected = match algorithm {
                        CongestionControl::Default => {
                            std::any::TypeId::of::<quinn::congestion::NewReno>()
                        }
                        CongestionControl::LowLatency => {
                            std::any::TypeId::of::<quinn::congestion::Bbr>()
                        }
                        CongestionControl::Throughput => {
                            std::any::TypeId::of::<quinn::congestion::Cubic>()
                        }
                    };
                    let builder = if transport_first {
                        builder()
                            .with_transport_config(transport)
                            .with_congestion_control(algorithm)
                    } else {
                        builder()
                            .with_congestion_control(algorithm)
                            .with_transport_config(transport)
                    };
                    let mut server = builder.with_certificate(chain, key).unwrap();
                    let client = crate::client::tests::builder()
                        .dangerous()
                        .with_no_certificate_verification()
                        .unwrap();
                    let url = format!("https://[::1]:{}/", server.local_addr().unwrap().port())
                        .parse::<url::Url>()
                        .unwrap();
                    let (client_session, server_session) =
                        tokio::join!(client.connect(url), async {
                            server.accept().await.unwrap().ok().await.unwrap()
                        });
                    let client_session = client_session.unwrap();
                    let controller = server_session.congestion_state().into_any();
                    assert_eq!(controller.as_ref().type_id(), expected);
                    assert_eq!(
                        built.load(Ordering::SeqCst),
                        usize::from(controller.is::<quinn::congestion::NewReno>())
                    );
                    server.close(0u32.into(), b"done");
                    client_session.closed().await;
                    server.wait_idle().await;
                }
            }
        })
        .await
        .unwrap();
    }
}
