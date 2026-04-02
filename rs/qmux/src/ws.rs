use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite;

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use crate::{alpn, Config, Error, Session, Version};

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

/// Wrap a pre-upgraded WebSocket connection as a client-side session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework. Pass the negotiated `sec-websocket-protocol`
/// header value (or `None` to default to the WebTransport wire format).
///
/// If `version` is `Some`, the QMux version is forced regardless of
/// the ALPN prefix. The ALPN is still used to extract the app protocol.
pub fn connect<T>(ws: T, alpn: Option<&str>, version: Option<Version>) -> Session
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let (version, protocol) = parse_alpn(alpn, version);
    let transport = WsTransport::new(ws);
    Session::connect(transport, Config::new(version, protocol))
}

/// Wrap a pre-upgraded WebSocket connection as a server-side session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework (e.g. axum). Pass the negotiated `sec-websocket-protocol`
/// header value (or `None` to default to the WebTransport wire format).
///
/// If `version` is `Some`, the QMux version is forced regardless of
/// the ALPN prefix. The ALPN is still used to extract the app protocol.
pub fn accept<T>(ws: T, alpn: Option<&str>, version: Option<Version>) -> Session
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let (version, protocol) = parse_alpn(alpn, version);
    let transport = WsTransport::new(ws);
    Session::accept(transport, Config::new(version, protocol))
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

        let transport = WsTransport::new(ws_stream);
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

        let transport = WsTransport::new(ws);
        Ok(Session::accept(transport, Config::new(version, protocol)))
    }
}
