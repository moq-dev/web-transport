use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite;

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use crate::{Error, Session, Version, PREFIX_QMUX, PREFIX_WEBTRANSPORT};

/// Wrap a pre-upgraded WebSocket connection as a client-side session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework. The `version` determines the wire format and
/// `protocol` should be the negotiated application-level subprotocol, if any.
pub fn connect<T>(ws: T, version: Version, protocol: Option<String>) -> Session
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let transport = WsTransport::new(ws);
    Session::connect(transport, version, protocol)
}

/// Wrap a pre-upgraded WebSocket connection as a server-side session.
///
/// Use this when the WebSocket handshake was already performed by an
/// external framework (e.g. axum). The `version` determines the wire format
/// and `protocol` should be the negotiated application-level subprotocol, if any.
pub fn accept<T>(ws: T, version: Version, protocol: Option<String>) -> Session
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let transport = WsTransport::new(ws);
    Session::accept(transport, version, protocol)
}

/// A QMux client that connects over WebSocket.
///
/// Supports both `webtransport.` and `qmux-00.` subprotocol prefixes,
/// preferring QMux when both are available.
#[derive(Default, Clone)]
pub struct Client {
    protocols: Vec<String>,
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

    /// Connect to a WebSocket server, negotiating the configured subprotocols.
    pub async fn connect(&self, url: &str) -> Result<Session, Error> {
        use tungstenite::{client::IntoClientRequest, http};

        for p in &self.protocols {
            validate_protocol(p)?;
        }

        let mut request = url.into_client_request().map_err(|_| Error::Closed)?;

        // Offer both prefix families, preferring qmux-00
        let protocol_value = if self.protocols.is_empty() {
            format!("{}, {}", crate::ALPN_QMUX, crate::ALPN_WEBTRANSPORT)
        } else {
            let qmux_prefixed: Vec<String> = self
                .protocols
                .iter()
                .map(|p| format!("{PREFIX_QMUX}{p}"))
                .collect();
            let wt_prefixed: Vec<String> = self
                .protocols
                .iter()
                .map(|p| format!("{PREFIX_WEBTRANSPORT}{p}"))
                .collect();
            format!(
                "{}, {}, {}, {}",
                crate::ALPN_QMUX,
                qmux_prefixed.join(", "),
                crate::ALPN_WEBTRANSPORT,
                wt_prefixed.join(", "),
            )
        };

        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            http::HeaderValue::from_str(&protocol_value)
                .map_err(|_| Error::InvalidProtocol(protocol_value))?,
        );

        let (ws_stream, response) =
            tokio_tungstenite::connect_async_with_config(request, None, false)
                .await
                .map_err(|_| Error::Closed)?;

        // Determine version and protocol from response
        let negotiated_header = response
            .headers()
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");

        let (version, protocol) = parse_negotiated_protocol(negotiated_header)?;

        let transport = WsTransport::new(ws_stream);
        Ok(Session::connect(transport, version, protocol))
    }
}

/// A QMux server that accepts WebSocket connections.
///
/// Supports both `webtransport.` and `qmux-00.` subprotocol prefixes,
/// preferring QMux when both are available.
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

            // Try qmux-00 prefix first
            let qmux_match = header_protocols
                .iter()
                .filter_map(|p| p.strip_prefix(PREFIX_QMUX))
                .find(|p| supported.iter().any(|s| s == p))
                .map(|p| p.to_string());

            if let Some(ref proto) = qmux_match {
                let response_value = format!("{PREFIX_QMUX}{proto}");
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(&response_value).unwrap(),
                );
                *negotiated_clone.lock().unwrap() =
                    Some((Version::QMux00, Some(proto.clone())));
                return Ok(response);
            }

            // Accept bare qmux-00 only when no specific protocols are configured.
            if supported.is_empty() && header_protocols.contains(&crate::ALPN_QMUX) {
                response.headers_mut().insert(
                    http::header::SEC_WEBSOCKET_PROTOCOL,
                    http::HeaderValue::from_str(crate::ALPN_QMUX).unwrap(),
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
        Ok(Session::accept(transport, version, protocol))
    }
}

/// Parse the negotiated WebSocket protocol header to determine version and app protocol.
fn parse_negotiated_protocol(header: &str) -> Result<(Version, Option<String>), Error> {
    for token in header.split(',').map(|p| p.trim()) {
        if let Some(proto) = token.strip_prefix(PREFIX_QMUX) {
            return Ok((Version::QMux00, Some(proto.to_string())));
        }
        if token == crate::ALPN_QMUX {
            return Ok((Version::QMux00, None));
        }
        if let Some(proto) = token.strip_prefix(PREFIX_WEBTRANSPORT) {
            return Ok((Version::WebTransport, Some(proto.to_string())));
        }
        if token == crate::ALPN_WEBTRANSPORT {
            return Ok((Version::WebTransport, None));
        }
    }

    if header.trim().is_empty() {
        // No protocol header — default to QMux
        Ok((Version::QMux00, None))
    } else {
        Err(Error::InvalidProtocol(header.to_string()))
    }
}
