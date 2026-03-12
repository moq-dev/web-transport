use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::StreamTransport;
use crate::{Error, Session, Version};

#[cfg(feature = "websocket")]
use crate::transport::WsTransport;

/// WebSocket protocol prefix constants.
#[cfg(feature = "websocket")]
const PREFIX_WEBTRANSPORT: &str = "webtransport.";
#[cfg(feature = "websocket")]
const PREFIX_QMUX: &str = "qmux-00.";

/// A QMux client that connects over TCP, TLS, or WebSocket.
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

    /// Connect over raw TCP. Always uses QMux protocol.
    pub async fn connect_tcp(&self, addr: impl ToSocketAddrs) -> Result<Session, Error> {
        let stream = TcpStream::connect(addr).await?;
        let transport = StreamTransport::new(stream);
        Ok(Session::new(transport, Version::QMux00, false, None))
    }

    /// Connect over TLS. Uses QMux ALPN negotiation.
    #[cfg(feature = "tls")]
    pub async fn connect_tls(
        &self,
        addr: impl ToSocketAddrs,
        config: std::sync::Arc<rustls::ClientConfig>,
    ) -> Result<Session, Error> {
        use tokio_rustls::TlsConnector;

        let stream = TcpStream::connect(&addr).await?;

        // Resolve the server name from the address for SNI
        let server_name = rustls::pki_types::ServerName::try_from("localhost")
            .map_err(|e| Error::Io(e.to_string()))?
            .to_owned();

        let connector = TlsConnector::from(config);
        let tls_stream = connector.connect(server_name, stream).await?;

        let transport = StreamTransport::new(tls_stream);
        Ok(Session::new(transport, Version::QMux00, false, None))
    }

    /// Connect over WebSocket. Supports both `webtransport.` and `qmux-00.` prefixes.
    #[cfg(feature = "websocket")]
    pub async fn connect_ws(&self, url: &str) -> Result<Session, Error> {
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http};

        for p in &self.protocols {
            crate::validate_protocol(p)?;
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

        let (version, protocol) = parse_negotiated_protocol(negotiated_header);

        let transport = WsTransport::new(ws_stream);
        Ok(Session::new(transport, version, false, protocol))
    }
}

/// Parse the negotiated WebSocket protocol header to determine version and app protocol.
#[cfg(feature = "websocket")]
fn parse_negotiated_protocol(header: &str) -> (Version, Option<String>) {
    for token in header.split(',').map(|p| p.trim()) {
        if let Some(proto) = token.strip_prefix(PREFIX_QMUX) {
            return (Version::QMux00, Some(proto.to_string()));
        }
        if token == crate::ALPN_QMUX {
            return (Version::QMux00, None);
        }
        if let Some(proto) = token.strip_prefix(PREFIX_WEBTRANSPORT) {
            return (Version::WebTransport, Some(proto.to_string()));
        }
        if token == crate::ALPN_WEBTRANSPORT {
            return (Version::WebTransport, None);
        }
    }
    // Default to QMux if nothing recognized
    (Version::QMux00, None)
}
