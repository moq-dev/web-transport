use std::sync::Arc;

use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{ConnectOptions, QuicTransportConfig},
};
use url::Url;

use crate::{ALPN_H3, ClientError, Session};

/// A client for connecting to an iroh WebTransport endpoint.
#[derive(Debug)]
pub struct Client {
    endpoint: Endpoint,
    config: QuicTransportConfig,
}

impl Client {
    /// Creates a client from an endpoint with the default transport config.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::with_transport_config(endpoint, Default::default())
    }

    /// Creates a client from an endpoint and a transport config.
    pub fn with_transport_config(endpoint: Endpoint, config: QuicTransportConfig) -> Self {
        Self { endpoint, config }
    }

    /// Connect to an iroh endpoint without HTTP/3.
    pub async fn connect_quic(
        &self,
        addr: impl Into<EndpointAddr>,
        alpn: &[u8],
    ) -> Result<Session, ClientError> {
        let conn = self.connect(addr, alpn).await?;
        Ok(Session::raw(conn))
    }

    /// Connect with a full HTTP/3 handshake and WebTransport semantics.
    ///
    /// Note that the url needs to have a `https:` scheme, otherwise the accepting side will
    /// fail to accept the connection.
    pub async fn connect_h3(
        &self,
        addr: impl Into<EndpointAddr>,
        url: Url,
    ) -> Result<Session, ClientError> {
        let conn = self.connect(addr, ALPN_H3.as_bytes()).await?;
        // Connect with the connection we established.
        Session::connect_h3(conn, url).await
    }

    async fn connect(
        &self,
        addr: impl Into<EndpointAddr>,
        alpn: &[u8],
    ) -> Result<iroh::endpoint::Connection, ClientError> {
        let opts = ConnectOptions::new().with_transport_config(self.config.clone());
        let conn = self
            .endpoint
            .connect_with_opts(addr, alpn, opts)
            .await
            .map_err(|err| ClientError::Connect(Arc::new(err.into())))?;
        let conn = conn
            .await
            .map_err(|err| ClientError::Connect(Arc::new(err.into())))?;
        Ok(conn)
    }

    /// Close the client endpoint.
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}
