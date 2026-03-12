use tokio::net::TcpStream;

use crate::transport::StreamTransport;
use crate::{Error, Session, Version};

#[cfg(feature = "websocket")]
use crate::transport::WsTransport;

/// WebSocket protocol prefix constants.
#[cfg(feature = "websocket")]
const PREFIX_WEBTRANSPORT: &str = "webtransport.";
#[cfg(feature = "websocket")]
const PREFIX_QMUX: &str = "qmux-00.";

/// A QMux server that accepts connections over TCP, TLS, or WebSocket.
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

    /// Accept a raw TCP connection. Always uses QMux protocol.
    pub async fn accept_tcp(&self, stream: TcpStream) -> Result<Session, Error> {
        let transport = StreamTransport::new(stream);
        Ok(Session::new(transport, Version::QMux00, true, None))
    }

    /// Accept a TLS connection. Uses QMux ALPN negotiation.
    #[cfg(feature = "tls")]
    pub async fn accept_tls(
        &self,
        stream: TcpStream,
        config: std::sync::Arc<rustls::ServerConfig>,
    ) -> Result<Session, Error> {
        use tokio_rustls::TlsAcceptor;

        let acceptor = TlsAcceptor::from(config);
        let tls_stream = acceptor.accept(stream).await?;

        let transport = StreamTransport::new(tls_stream);
        Ok(Session::new(transport, Version::QMux00, true, None))
    }

    /// Accept a WebSocket connection. Supports both `webtransport.` and `qmux-00.` prefixes,
    /// preferring QMux when both are available.
    #[cfg(feature = "websocket")]
    pub async fn accept_ws<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
        &self,
        socket: T,
    ) -> Result<Session, Error> {
        use std::sync::{Arc, Mutex};
        use tokio_tungstenite::tungstenite::{handshake::server, http};

        for p in &self.protocols {
            crate::validate_protocol(p)?;
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

            // Check for bare qmux-00
            if header_protocols.contains(&crate::ALPN_QMUX) && supported.is_empty() {
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

            // Check for bare webtransport
            if header_protocols.contains(&crate::ALPN_WEBTRANSPORT) {
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
            .unwrap_or((Version::QMux00, None));

        let transport = WsTransport::new(ws);
        Ok(Session::new(transport, version, true, protocol))
    }
}
