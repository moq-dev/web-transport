//! WebTransport server wrapped for UniFFI.
//!
//! Mirrors `rs/web-transport-python/src/server.rs`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::client::{build_server_tls, CongestionControl};
use crate::error::{map_server_error, WebTransportError};
use crate::ffi::RUNTIME;
use crate::session::{RemoteAddress, Session};

/// Server configuration record.
///
/// `certificate_chain` is a list of DER-encoded certs (leaf first), and
/// `private_key` is the DER-encoded private key. `bind` accepts any address
/// quinn understands, e.g. `"[::]:4433"` or `"0.0.0.0:4433"`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerConfig {
    pub certificate_chain: Vec<Vec<u8>>,
    pub private_key: Vec<u8>,
    #[uniffi(default = "[::]:4433")]
    pub bind: String,
    pub congestion_control: CongestionControl,
    #[uniffi(default = Some(30.0))]
    pub max_idle_timeout_secs: Option<f64>,
    #[uniffi(default = None)]
    pub keep_alive_interval_secs: Option<f64>,
}

#[derive(uniffi::Object)]
pub struct Server {
    inner: Arc<Mutex<web_transport_quinn::Server>>,
    endpoint: quinn::Endpoint,
    local_addr: RemoteAddress,
    transport_config: Arc<quinn::TransportConfig>,
}

#[uniffi::export]
impl Server {
    #[uniffi::constructor]
    pub fn new(config: ServerConfig) -> Result<Arc<Self>, WebTransportError> {
        let addr: SocketAddr = config
            .bind
            .parse()
            .map_err(|e| WebTransportError::invalid(format!("invalid bind address: {e}")))?;

        let tls = build_server_tls(config.certificate_chain.clone(), config.private_key.clone())?;

        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(
            config
                .max_idle_timeout_secs
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| WebTransportError::invalid("invalid max_idle_timeout_secs"))?
                .map(quinn::IdleTimeout::try_from)
                .transpose()
                .map_err(|e| WebTransportError::invalid(format!("invalid idle timeout: {e}")))?,
        );
        transport.keep_alive_interval(
            config
                .keep_alive_interval_secs
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| WebTransportError::invalid("invalid keep_alive_interval_secs"))?,
        );
        match config.congestion_control {
            CongestionControl::Default => {}
            CongestionControl::Throughput => {
                transport.congestion_controller_factory(Arc::new(
                    quinn::congestion::CubicConfig::default(),
                ));
            }
            CongestionControl::LowLatency => {
                transport.congestion_controller_factory(Arc::new(
                    quinn::congestion::BbrConfig::default(),
                ));
            }
        }
        let transport_config = Arc::new(transport);

        let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
            .map_err(|e| WebTransportError::invalid(format!("QUIC config: {e}")))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic));
        server_config.transport_config(transport_config.clone());

        let _guard = RUNTIME.enter();
        let endpoint = quinn::Endpoint::server(server_config, addr)
            .map_err(|e| WebTransportError::Io(format!("failed to bind: {e}")))?;

        let local_addr = endpoint
            .local_addr()
            .map_err(|e| WebTransportError::Io(format!("local_addr: {e}")))?;

        let server = web_transport_quinn::Server::new(endpoint.clone());

        Ok(Arc::new(Self {
            inner: Arc::new(Mutex::new(server)),
            endpoint,
            local_addr: RemoteAddress {
                host: local_addr.ip().to_string(),
                port: local_addr.port(),
            },
            transport_config,
        }))
    }

    /// Wait for the next incoming session request.
    ///
    /// Returns `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<Arc<SessionRequest>> {
        let inner = self.inner.clone();
        let handle = RUNTIME.spawn(async move {
            let mut guard = inner.lock().await;
            guard.accept().await
        });
        let req = handle.await.ok().flatten()?;
        Some(SessionRequest::new(req))
    }

    /// Close all connections.
    #[uniffi::method(default(code = 0, reason = ""))]
    pub fn close(&self, code: u64, reason: String) -> Result<(), WebTransportError> {
        let var_code = quinn::VarInt::from_u64(code)
            .map_err(|_| WebTransportError::invalid("code must be < 2^62"))?;
        let _guard = RUNTIME.enter();
        self.endpoint.close(var_code, reason.as_bytes());
        Ok(())
    }

    /// Wait for all connections to shut down cleanly.
    pub async fn wait_closed(&self) {
        let endpoint = self.endpoint.clone();
        let handle = RUNTIME.spawn(async move {
            endpoint.wait_idle().await;
        });
        let _ = handle.await;
    }

    /// The local `(host, port)` the server is bound to.
    pub fn local_addr(&self) -> RemoteAddress {
        self.local_addr.clone()
    }

    /// Replace the TLS certificate for new incoming connections.
    pub fn reload_certificates(
        &self,
        certificate_chain: Vec<Vec<u8>>,
        private_key: Vec<u8>,
    ) -> Result<(), WebTransportError> {
        let tls = build_server_tls(certificate_chain, private_key)?;
        let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
            .map_err(|e| WebTransportError::invalid(format!("QUIC config: {e}")))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic));
        server_config.transport_config(self.transport_config.clone());
        self.endpoint.set_server_config(Some(server_config));
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _guard = RUNTIME.enter();
        self.endpoint.close(quinn::VarInt::from_u32(0), b"");
    }
}

#[derive(uniffi::Object)]
pub struct SessionRequest {
    inner: tokio::sync::Mutex<Option<web_transport_quinn::Request>>,
    url: String,
    remote_address: RemoteAddress,
}

impl SessionRequest {
    pub fn new(request: web_transport_quinn::Request) -> Arc<Self> {
        let url = request.url.to_string();
        let addr = request.conn().remote_address();
        Arc::new(Self {
            inner: tokio::sync::Mutex::new(Some(request)),
            url,
            remote_address: RemoteAddress {
                host: addr.ip().to_string(),
                port: addr.port(),
            },
        })
    }
}

#[uniffi::export]
impl SessionRequest {
    /// The URL requested by the client.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// The remote peer's `(host, port)`.
    pub fn remote_address(&self) -> RemoteAddress {
        self.remote_address.clone()
    }

    /// Accept the session request.
    pub async fn accept(&self) -> Result<Arc<Session>, WebTransportError> {
        let request = {
            let mut guard = self.inner.lock().await;
            guard.take().ok_or_else(|| {
                WebTransportError::protocol("request already accepted or rejected")
            })?
        };
        let handle = RUNTIME.spawn(async move { request.ok().await.map_err(map_server_error) });
        let session = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("accept task: {e}")))??;
        Ok(Session::new(session))
    }

    /// Reject the session request with an HTTP status code.
    #[uniffi::method(default(status_code = 404))]
    pub async fn reject(&self, status_code: u16) -> Result<(), WebTransportError> {
        let request = {
            let mut guard = self.inner.lock().await;
            guard.take().ok_or_else(|| {
                WebTransportError::protocol("request already accepted or rejected")
            })?
        };
        let status = http::StatusCode::from_u16(status_code)
            .map_err(|e| WebTransportError::invalid(format!("invalid status code: {e}")))?;
        let handle = RUNTIME.spawn(async move {
            // Use respond() rather than reject(): respond() keeps the QUIC
            // connection alive long enough to transmit the rejection HTTP
            // response, then we close() from our side.
            let session = request.respond(status).await.map_err(map_server_error)?;
            session.close(0, b"");
            Ok::<_, WebTransportError>(())
        });
        handle
            .await
            .map_err(|e| WebTransportError::Io(format!("reject task: {e}")))??;
        Ok(())
    }
}
