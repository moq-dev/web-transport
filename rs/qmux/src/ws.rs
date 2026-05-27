use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite;

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use crate::{alpn, Config, Error, Session, Version};

/// Keep-alive configuration for WebSocket transports.
///
/// WebSocket has no built-in idle timeout: when the peer's host crashes
/// or its network drops without sending a TCP FIN, the local socket
/// stays "open" until OS-level TCP keep_alive eventually probes — typically
/// hours. Set this to send periodic Pings and close the session if no
/// frame arrives within `timeout`.
#[derive(Debug, Clone, Copy)]
pub struct KeepAlive {
    /// How often to send a Ping frame to the peer.
    pub interval: Duration,

    /// Close the session if no frame is received from the peer within this window.
    /// Should be a small multiple of `interval` to tolerate transient drops.
    pub timeout: Duration,
}

impl KeepAlive {
    /// Create a keep-alive config with the given interval and timeout.
    pub fn new(interval: Duration, timeout: Duration) -> Self {
        Self { interval, timeout }
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        // Match the QUIC defaults used by moq-native: 5s ping, 30s deadline.
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Extract the application protocol from a negotiated WebSocket subprotocol header.
///
/// The QMux version is fixed by the caller via [`Client::new`] / [`Server::new`]
/// (or [`Upgraded::new`]); this function just strips the expected prefix to recover
/// the app protocol, if any. Accepts the bare version ALPN (returns `None`) and
/// the prefixed form `{version.prefix()}{proto}`. Unknown values yield `None`.
fn parse_protocol(alpn: Option<&str>, version: Version) -> Option<String> {
    let alpn = alpn.filter(|s| !s.is_empty())?;
    if alpn == version.alpn() {
        return None;
    }
    let proto = alpn.strip_prefix(version.prefix())?;
    (!proto.is_empty()).then(|| proto.to_string())
}

/// Wrap an already-upgraded WebSocket as a QMux session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework (e.g. axum) or by caller-driven code that needs to
/// run its own handshake (e.g. to advertise multiple QMux versions in one
/// `Sec-WebSocket-Protocol` header). The caller picks the QMux version at
/// construction; pass the negotiated `sec-websocket-protocol` value with
/// [`Upgraded::with_alpn`] so the application protocol can be recovered.
pub struct Upgraded<T> {
    ws: T,
    alpn: Option<String>,
    version: Version,
    keep_alive: Option<KeepAlive>,
}

impl<T> Upgraded<T>
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    pub fn new(ws: T, version: Version) -> Self {
        Self {
            ws,
            alpn: None,
            version,
            keep_alive: None,
        }
    }

    /// Set the negotiated `sec-websocket-protocol` value from the handshake.
    pub fn with_alpn(mut self, alpn: &str) -> Self {
        self.alpn = Some(alpn.to_string());
        self
    }

    /// Drive a keep-alive Ping/timeout on the WebSocket.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Wrap as a client-side session.
    pub fn connect(self) -> Session {
        let protocol = parse_protocol(self.alpn.as_deref(), self.version);
        let version = self.version;
        Session::connect(self.into_transport(), Config::new(version, protocol))
    }

    /// Wrap as a server-side session.
    pub fn accept(self) -> Session {
        let protocol = parse_protocol(self.alpn.as_deref(), self.version);
        let version = self.version;
        Session::accept(self.into_transport(), Config::new(version, protocol))
    }

    fn into_transport(self) -> WsTransport<T> {
        let transport = WsTransport::new(self.ws);
        match self.keep_alive {
            Some(ka) => transport.with_keep_alive(ka),
            None => transport,
        }
    }
}

/// A QMux client that connects over WebSocket.
///
/// The QMux version is fixed at construction. The client advertises the bare
/// version ALPN plus each configured application protocol prefixed by
/// `{version.prefix()}`. Callers that want to negotiate multiple QMux versions
/// (e.g. as a fallback) should drive separate connection attempts; this crate
/// does not cross-product versions on its own.
#[derive(Clone)]
pub struct Client {
    version: Version,
    protocols: Vec<String>,
    config: Option<tungstenite::protocol::WebSocketConfig>,
    keep_alive: Option<KeepAlive>,
    #[cfg(feature = "wss")]
    connector: Option<tokio_tungstenite::Connector>,
}

impl Client {
    /// Create a client pinned to a specific QMux version.
    pub fn new(version: Version) -> Self {
        Self {
            version,
            protocols: Vec::new(),
            config: None,
            keep_alive: None,
            #[cfg(feature = "wss")]
            connector: None,
        }
    }

    /// Add a supported application-level subprotocol for negotiation.
    pub fn with_protocol(mut self, protocol: &str) -> Self {
        self.protocols.push(protocol.to_string());
        self
    }

    /// Add multiple supported application-level subprotocols for negotiation.
    pub fn with_protocols(mut self, protocols: &[&str]) -> Self {
        self.protocols
            .extend(protocols.iter().map(|s| s.to_string()));
        self
    }

    /// Set the WebSocket configuration (e.g. max message/frame sizes).
    pub fn with_config(mut self, config: tungstenite::protocol::WebSocketConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Send periodic Pings and close the session if the peer goes silent.
    ///
    /// WebSocket has no built-in idle timeout, so without this a crashed peer
    /// stays "connected" until OS-level TCP keep_alive eventually probes.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Set the TLS connector for secure WebSocket connections.
    #[cfg(feature = "wss")]
    pub fn with_connector(mut self, connector: tokio_tungstenite::Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// Connect to a WebSocket server, negotiating the configured subprotocols.
    pub async fn connect(&self, url: &str) -> Result<Session, Error> {
        use tungstenite::{client::IntoClientRequest, http};

        for p in &self.protocols {
            validate_protocol(p)?;
        }

        let mut request = url
            .into_client_request()
            .map_err(|e| Error::Io(e.to_string()))?;

        let protocol_value = alpn::build(self.version, &self.protocols).join(", ");

        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            http::HeaderValue::from_str(&protocol_value)
                .map_err(|_| Error::InvalidProtocol(protocol_value))?,
        );

        #[cfg(feature = "wss")]
        let (ws_stream, response) = {
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                self.config,
                false,
                self.connector.clone(),
            )
            .await
            .map_err(|e| Error::Io(e.to_string()))?
        };

        #[cfg(not(feature = "wss"))]
        let (ws_stream, response) =
            tokio_tungstenite::connect_async_with_config(request, self.config, false)
                .await
                .map_err(|e| Error::Io(e.to_string()))?;

        let negotiated = response
            .headers()
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|h| h.to_str().ok());

        let protocol = parse_protocol(negotiated, self.version);

        let transport = match self.keep_alive {
            Some(ka) => WsTransport::new(ws_stream).with_keep_alive(ka),
            None => WsTransport::new(ws_stream),
        };
        Ok(Session::connect(
            transport,
            Config::new(self.version, protocol),
        ))
    }
}

/// A QMux server that accepts WebSocket connections.
///
/// The QMux version is fixed at construction. Only ALPNs matching this version
/// are accepted. To support multiple QMux versions on the same port, run more
/// than one Server instance.
#[derive(Clone)]
pub struct Server {
    version: Version,
    protocols: Vec<String>,
    keep_alive: Option<KeepAlive>,
}

impl Server {
    /// Create a server pinned to a specific QMux version.
    pub fn new(version: Version) -> Self {
        Self {
            version,
            protocols: Vec::new(),
            keep_alive: None,
        }
    }

    /// Add a supported application-level subprotocol for negotiation.
    pub fn with_protocol(mut self, protocol: &str) -> Self {
        self.protocols.push(protocol.to_string());
        self
    }

    /// Add multiple supported application-level subprotocols for negotiation.
    pub fn with_protocols(mut self, protocols: &[&str]) -> Self {
        self.protocols
            .extend(protocols.iter().map(|s| s.to_string()));
        self
    }

    /// Send periodic Pings and close the session if the peer goes silent.
    ///
    /// WebSocket has no built-in idle timeout, so without this a crashed peer
    /// stays "connected" until OS-level TCP keep_alive eventually probes.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Accept a WebSocket connection, negotiating the subprotocol.
    pub async fn accept<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        &self,
        socket: T,
    ) -> Result<Session, Error> {
        use std::sync::{Arc, Mutex};
        use tungstenite::{handshake::server, http};

        for p in &self.protocols {
            validate_protocol(p)?;
        }

        let negotiated = Arc::new(Mutex::new(None::<Option<String>>));
        let negotiated_clone = negotiated.clone();
        let supported = self.protocols.clone();
        let version = self.version;
        let prefix = version.prefix();
        let bare_alpn = version.alpn();

        #[allow(clippy::result_large_err)]
        let callback = move |req: &server::Request,
                             mut response: server::Response|
              -> Result<server::Response, server::ErrorResponse> {
            let header_protocols: Vec<&str> = req
                .headers()
                .get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|h| h.split(','))
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();

            // Try prefixed protocol match first.
            if let Some(proto) = header_protocols
                .iter()
                .filter_map(|p| p.strip_prefix(prefix))
                .find(|p| supported.iter().any(|s| s == p))
                .map(|p| p.to_string())
            {
                let response_value = format!("{prefix}{proto}");
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(&response_value).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some(Some(proto));
                return Ok(response);
            }

            // Fallback: accept the bare version ALPN only when no specific
            // protocols are configured.
            if supported.is_empty() && header_protocols.contains(&bare_alpn) {
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(bare_alpn).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some(None);
                return Ok(response);
            }

            Err(http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(Some("no supported protocol".to_string()))
                .unwrap())
        };

        let ws = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, None).await?;

        let protocol = negotiated
            .lock()
            .unwrap()
            .take()
            .expect("negotiated must be set after successful handshake");

        let transport = match self.keep_alive {
            Some(ka) => WsTransport::new(ws).with_keep_alive(ka),
            None => WsTransport::new(ws),
        };
        Ok(Session::accept(
            transport,
            Config::new(self.version, protocol),
        ))
    }
}
