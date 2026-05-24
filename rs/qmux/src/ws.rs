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

/// Parse a negotiated WebSocket subprotocol header into a version and app protocol.
///
/// Supports bare ALPNs (`"qmux-01"`, `"qmux-00"`, `"webtransport"`) and
/// prefixed variants (`"qmux-01.moq-03"`, `"qmux-00.moq-03"`, `"webtransport.moq-03"`).
///
/// If `pin` is `Some`, the version is forced regardless of the ALPN prefix.
/// Defaults to `WebTransport` when `None` or empty and no pin is set.
fn parse_alpn(alpn: Option<&str>, pin: Option<Version>) -> (Version, Option<String>) {
    let alpn = match alpn {
        Some(s) if !s.is_empty() => s,
        _ => return (pin.unwrap_or(Version::WebTransport), None),
    };

    for &version in Version::ALL {
        if alpn == version.alpn() {
            return (pin.unwrap_or(version), None);
        }
        if let Some(proto) = alpn.strip_prefix(version.prefix()) {
            let proto = if proto.is_empty() {
                None
            } else {
                Some(proto.to_string())
            };
            return (pin.unwrap_or(version), proto);
        }
    }

    // Unknown
    (pin.unwrap_or(Version::WebTransport), None)
}

/// Wrap a pre-upgraded WebSocket connection as a session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework (e.g. axum). Set the negotiated
/// `sec-websocket-protocol` header value with [`Bare::with_alpn`];
/// without it, the WebTransport wire format is used.
pub struct Bare<T> {
    ws: T,
    alpn: Option<String>,
    version: Option<Version>,
    keep_alive: Option<KeepAlive>,
}

impl<T> Bare<T>
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    pub fn new(ws: T) -> Self {
        Self {
            ws,
            alpn: None,
            version: None,
            keep_alive: None,
        }
    }

    /// Set the negotiated `sec-websocket-protocol` value from the handshake.
    pub fn with_alpn(mut self, alpn: &str) -> Self {
        self.alpn = Some(alpn.to_string());
        self
    }

    /// Pin a specific QMux version, ignoring the negotiated ALPN's prefix.
    ///
    /// The ALPN is still used to extract the application protocol.
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Drive a keep-alive Ping/timeout on the WebSocket.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Wrap as a client-side session.
    pub fn connect(self) -> Session {
        let (version, protocol) = parse_alpn(self.alpn.as_deref(), self.version);
        Session::connect(self.into_transport(), Config::new(version, protocol))
    }

    /// Wrap as a server-side session.
    pub fn accept(self) -> Session {
        let (version, protocol) = parse_alpn(self.alpn.as_deref(), self.version);
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
/// By default, supports `qmux-01.`, `qmux-00.`, and `webtransport.` subprotocol
/// prefixes, preferring the latest QMux version. Use [`with_version`](Client::with_version)
/// to pin a specific QMux version.
#[derive(Default, Clone)]
pub struct Client {
    protocols: Vec<String>,
    version: Option<Version>,
    config: Option<tungstenite::protocol::WebSocketConfig>,
    keep_alive: Option<KeepAlive>,
    #[cfg(feature = "wss")]
    connector: Option<tokio_tungstenite::Connector>,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin a specific QMux version for this client.
    ///
    /// When set, only ALPNs for this version are offered during negotiation,
    /// and the session always uses this version regardless of the negotiated ALPN.
    ///
    /// For example, if `moq-transport-17` always uses QMux draft-00:
    /// ```ignore
    /// let client = Client::new()
    ///     .with_version(Version::QMux00)
    ///     .with_protocol("moq-transport-17");
    /// ```
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
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

        let versions: Vec<Version> = self.version.into_iter().collect();
        let protocol_value = alpn::build(&self.protocols, &versions).join(", ");

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

        // Determine version and protocol from response
        let negotiated = response
            .headers()
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|h| h.to_str().ok());

        let (version, protocol) = parse_alpn(negotiated, self.version);

        let transport = match self.keep_alive {
            Some(ka) => WsTransport::new(ws_stream).with_keep_alive(ka),
            None => WsTransport::new(ws_stream),
        };
        Ok(Session::connect(transport, Config::new(version, protocol)))
    }
}

/// A QMux server that accepts WebSocket connections.
///
/// By default, supports `qmux-01.`, `qmux-00.`, and `webtransport.` subprotocol
/// prefixes, preferring the latest QMux version. Use [`with_version`](Server::with_version)
/// to pin a specific QMux version.
#[derive(Default, Clone)]
pub struct Server {
    protocols: Vec<String>,
    version: Option<Version>,
    keep_alive: Option<KeepAlive>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin a specific QMux version for this server.
    ///
    /// When set, only ALPNs for this version are accepted during negotiation,
    /// and the session always uses this version regardless of the negotiated ALPN.
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
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

        let negotiated = Arc::new(Mutex::new(None::<(Version, Option<String>)>));
        let negotiated_clone = negotiated.clone();
        let supported = self.protocols.clone();
        let pin_version = self.version;

        // Determine which versions to try, in preference order
        let versions: Vec<Version> = match pin_version {
            Some(v) => vec![v],
            None => Version::ALL.to_vec(),
        };

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

            for &version in &versions {
                let prefix = version.prefix();
                let version = pin_version.unwrap_or(version);

                // Try prefixed protocol match
                let matched = header_protocols
                    .iter()
                    .filter_map(|p| p.strip_prefix(prefix))
                    .find(|p| supported.iter().any(|s| s == p))
                    .map(|p| p.to_string());

                if let Some(ref proto) = matched {
                    let response_value = format!("{prefix}{proto}");
                    response.headers_mut().insert(
                        http::header::SEC_WEBSOCKET_PROTOCOL,
                        http::HeaderValue::from_str(&response_value).unwrap(),
                    );
                    *negotiated_clone.lock().unwrap() = Some((version, Some(proto.clone())));
                    return Ok(response);
                }

                // Accept bare version ALPN only when no specific protocols are configured.
                let bare_alpn = pin_version.unwrap_or(version).alpn();
                if supported.is_empty() && header_protocols.contains(&bare_alpn) {
                    response.headers_mut().insert(
                        http::header::SEC_WEBSOCKET_PROTOCOL,
                        http::HeaderValue::from_str(bare_alpn).unwrap(),
                    );
                    *negotiated_clone.lock().unwrap() = Some((version, None));
                    return Ok(response);
                }
            }

            Err(http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(Some("no supported protocol".to_string()))
                .unwrap())
        };

        let ws = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, None).await?;

        let (version, protocol) = negotiated
            .lock()
            .unwrap()
            .take()
            .expect("negotiated must be set after successful handshake");

        let transport = match self.keep_alive {
            Some(ka) => WsTransport::new(ws).with_keep_alive(ka),
            None => WsTransport::new(ws),
        };
        Ok(Session::accept(transport, Config::new(version, protocol)))
    }
}
