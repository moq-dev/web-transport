use std::ops::Deref;
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use std::sync::Arc;

use futures::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use http::{header::GetAll, HeaderMap, HeaderValue};
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use url::Url;

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use crate::{crypto, CongestionControl};
use crate::{protocol_negotation, Connect, ServerError, Session, Settings};

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
/// Construct a WebTransport [Server] using sane defaults.
///
/// This is optional; advanced users may use [Server::new] directly.
pub struct ServerBuilder {
    provider: crypto::Provider,
    addr: std::net::SocketAddr,
    congestion_controller:
        Option<Arc<dyn quinn::congestion::ControllerFactory + Send + Sync + 'static>>,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl ServerBuilder {
    /// Create a server builder with sane defaults.
    pub fn new() -> Self {
        Self {
            provider: crypto::default_provider(),
            addr: "[::]:443".parse().unwrap(),
            congestion_controller: None,
        }
    }

    /// Listen on the specified address.
    pub fn with_addr(self, addr: std::net::SocketAddr) -> Self {
        Self { addr, ..self }
    }

    /// Enable the specified congestion controller.
    pub fn with_congestion_control(mut self, algorithm: CongestionControl) -> Self {
        self.congestion_controller = match algorithm {
            CongestionControl::LowLatency => {
                Some(Arc::new(quinn::congestion::BbrConfig::default()))
            }
            // TODO BBR is also higher throughput in theory.
            CongestionControl::Throughput => {
                Some(Arc::new(quinn::congestion::CubicConfig::default()))
            }
            CongestionControl::Default => None,
        };

        self
    }

    /// Supply a certificate used for TLS.
    // TODO support multiple certs based on...?
    pub fn with_certificate(
        self,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Server, ServerError> {
        // Standard Quinn setup
        let mut config = rustls::ServerConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(chain, key)?;

        config.alpn_protocols = vec![crate::ALPN.as_bytes().to_vec()]; // this one is important

        let config: quinn::crypto::rustls::QuicServerConfig = config.try_into().unwrap();
        let config = quinn::ServerConfig::with_crypto(Arc::new(config));

        let server = quinn::Endpoint::server(config, self.addr)
            .map_err(|e| ServerError::IoError(e.into()))?;

        Ok(Server::new(server))
    }
}

/// A WebTransport server that accepts new sessions.
pub struct Server {
    endpoint: quinn::Endpoint,
    accept: FuturesUnordered<BoxFuture<'static, Result<Request, ServerError>>>,
}

impl Deref for Server {
    type Target = quinn::Endpoint;

    fn deref(&self) -> &Self::Target {
        &self.endpoint
    }
}

impl Server {
    /// Manaully create a new server with a manually constructed Endpoint.
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
            tokio::select! {
                res = self.endpoint.accept() => {
                    let conn = res?;
                    log::debug!("new connection request {}", conn.remote_address());
                    self.accept.push(Box::pin(async move {
                        let conn = conn.await?;
                        let addr = conn.remote_address();
                        Request::accept(conn).await.inspect_err(|err| {
                            log::error!("[{addr}] Error accepting connection: {err}");
                        })
                    }));
                }
                Some(res) = self.accept.next() => {
                    if let Ok(session) = res {
                        return Some(session)
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
    connect: Connect,
}

impl Request {
    /// Accept a new WebTransport session from a client.
    pub async fn accept(conn: quinn::Connection) -> Result<Self, ServerError> {
        log::debug!("[{}] New connection request", conn.remote_address());
        // Perform the H3 handshake by sending/reciving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;
        log::debug!("[{}] Settings handled", conn.remote_address());

        // Accept the CONNECT request but don't send a response yet.
        let connect = Connect::accept(&conn).await?;
        log::debug!(
            "[{}] New connection  accepted  {:?}",
            conn.remote_address(),
            connect.url()
        );

        // Return the resulting request with a reference to the settings/connect streams.
        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    /// Returns the URL provided by the client.
    pub fn url(&self) -> &Url {
        self.connect.url()
    }

    pub fn subprotocols(&self) -> GetAll<HeaderValue> {
        self.connect
            .request
            .headers
            .get_all(protocol_negotation::AVAILABLE_NAME)
    }

    /// Accept the session, returning a 200 OK.
    pub async fn ok(mut self) -> Result<Session, ServerError> {
        self.connect.respond(http::StatusCode::OK).await?;
        Ok(Session::new(self.conn, self.settings, self.connect))
    }

    /// Accept the session, returning a 200 OK.
    pub async fn ok_with_subprotocol(
        mut self,
        selected_protocol: HeaderValue,
    ) -> Result<Session, ServerError> {
        let mut headers = HeaderMap::new();
        headers.insert(protocol_negotation::SELECTED_NAME, selected_protocol);
        self.connect
            .respond_with_headers(http::StatusCode::OK, headers)
            .await?;
        Ok(Session::new(self.conn, self.settings, self.connect))
    }

    /// Reject the session, returing your favorite HTTP status code.
    pub async fn close(mut self, status: http::StatusCode) -> Result<(), ServerError> {
        self.connect.respond(status).await?;
        Ok(())
    }
}
