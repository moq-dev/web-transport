use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite;

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use crate::{
    alpn, Config, Error, Session, Version, PREFIX_QMUX_00, PREFIX_QMUX_01, PREFIX_WEBTRANSPORT,
};

/// Parse a negotiated WebSocket subprotocol header into a version and app protocol.
///
/// Supports bare ALPNs (`"qmux-01"`, `"qmux-00"`, `"webtransport"`) and
/// prefixed variants (`"qmux-01.moq-03"`, `"qmux-00.moq-03"`, `"webtransport.moq-03"`).
/// Defaults to `WebTransport` when `None` or empty.
fn parse_alpn(alpn: Option<&str>) -> (Version, Option<String>) {
    let alpn = match alpn {
        Some(s) if !s.is_empty() => s,
        _ => return (Version::WebTransport, None),
    };

    if let Some(proto) = alpn.strip_prefix(PREFIX_QMUX_01) {
        let proto = if proto.is_empty() {
            None
        } else {
            Some(proto.to_string())
        };
        return (Version::QMux01, proto);
    }
    if alpn == crate::ALPN_QMUX_01 {
        return (Version::QMux01, None);
    }
    if let Some(proto) = alpn.strip_prefix(PREFIX_QMUX_00) {
        let proto = if proto.is_empty() {
            None
        } else {
            Some(proto.to_string())
        };
        return (Version::QMux00, proto);
    }
    if alpn == crate::ALPN_QMUX_00 {
        return (Version::QMux00, None);
    }
    if let Some(proto) = alpn.strip_prefix(PREFIX_WEBTRANSPORT) {
        let proto = if proto.is_empty() {
            None
        } else {
            Some(proto.to_string())
        };
        return (Version::WebTransport, proto);
    }

    // Unknown or bare "webtransport"
    (Version::WebTransport, None)
}

/// Wrap a pre-upgraded WebSocket connection as a client-side session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework. Pass the negotiated `sec-websocket-protocol`
/// header value (or `None` to default to the WebTransport wire format).
pub fn connect<T>(ws: T, alpn: Option<&str>) -> Session
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let (version, protocol) = parse_alpn(alpn);
    let transport = WsTransport::new(ws);
    Session::connect(transport, Config::new(version, protocol))
}

/// Wrap a pre-upgraded WebSocket connection as a server-side session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework (e.g. axum). Pass the negotiated `sec-websocket-protocol`
/// header value (or `None` to default to the WebTransport wire format).
pub fn accept<T>(ws: T, alpn: Option<&str>) -> Session
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let (version, protocol) = parse_alpn(alpn);
    let transport = WsTransport::new(ws);
    Session::accept(transport, Config::new(version, protocol))
}

/// A QMux client that connects over WebSocket.
///
/// Supports `qmux-01.`, `qmux-00.`, and `webtransport.` subprotocol prefixes,
/// preferring the latest QMux version when available.
#[derive(Default, Clone)]
pub struct Client {
    protocols: Vec<String>,
    config: Option<tungstenite::protocol::WebSocketConfig>,
    #[cfg(feature = "wss")]
    connector: Option<tokio_tungstenite::Connector>,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
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

        // Offer both prefix families, preferring qmux-00
        let protocol_value = alpn::build(&self.protocols).join(", ");

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

        let (version, protocol) = parse_alpn(negotiated);

        let transport = WsTransport::new(ws_stream);
        Ok(Session::connect(transport, Config::new(version, protocol)))
    }
}

/// A QMux server that accepts WebSocket connections.
///
/// Supports `qmux-01.`, `qmux-00.`, and `webtransport.` subprotocol prefixes,
/// preferring the latest QMux version when available.
#[derive(Default, Clone)]
pub struct Server {
    protocols: Vec<String>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
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

            // Try qmux-01 prefix first
            let qmux01_match = header_protocols
                .iter()
                .filter_map(|p| p.strip_prefix(PREFIX_QMUX_01))
                .find(|p| supported.iter().any(|s| s == p))
                .map(|p| p.to_string());

            if let Some(ref proto) = qmux01_match {
                let response_value = format!("{PREFIX_QMUX_01}{proto}");
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(&response_value).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some((Version::QMux01, Some(proto.clone())));
                return Ok(response);
            }

            // Accept bare qmux-01 only when no specific protocols are configured.
            if supported.is_empty() && header_protocols.contains(&crate::ALPN_QMUX_01) {
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(crate::ALPN_QMUX_01).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some((Version::QMux01, None));
                return Ok(response);
            }

            // Try qmux-00 prefix
            let qmux00_match = header_protocols
                .iter()
                .filter_map(|p| p.strip_prefix(PREFIX_QMUX_00))
                .find(|p| supported.iter().any(|s| s == p))
                .map(|p| p.to_string());

            if let Some(ref proto) = qmux00_match {
                let response_value = format!("{PREFIX_QMUX_00}{proto}");
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(&response_value).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some((Version::QMux00, Some(proto.clone())));
                return Ok(response);
            }

            // Accept bare qmux-00 only when no specific protocols are configured.
            if supported.is_empty() && header_protocols.contains(&crate::ALPN_QMUX_00) {
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(crate::ALPN_QMUX_00).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some((Version::QMux00, None));
                return Ok(response);
            }

            // Fall back to webtransport prefix
            let wt_match = header_protocols
                .iter()
                .filter_map(|p| p.strip_prefix(PREFIX_WEBTRANSPORT))
                .find(|p| supported.iter().any(|s| s == p))
                .map(|p| p.to_string());

            if let Some(ref proto) = wt_match {
                let response_value = format!("{PREFIX_WEBTRANSPORT}{proto}");
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(&response_value).unwrap(),
                );
                *negotiated_clone.lock().unwrap() =
                    Some((Version::WebTransport, Some(proto.clone())));
                return Ok(response);
            }

            // Check for bare webtransport only when no specific protocols are configured.
            if supported.is_empty() && header_protocols.contains(&crate::ALPN_WEBTRANSPORT) {
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(crate::ALPN_WEBTRANSPORT).unwrap(),
                );
                *negotiated_clone.lock().unwrap() = Some((Version::WebTransport, None));
                return Ok(response);
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
