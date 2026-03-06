use std::sync::Arc;

use crate::{tungstenite, Session, ALPN};
use tokio::io::{AsyncRead, AsyncWrite};
use tungstenite::{handshake::server, http};

use crate::Error;

/// The prefix required for all application subprotocols over this WebSocket compatibility layer.
const PREFIX: &str = "webtransport:";

/// A WebTransport server that accepts WebSocket connections.
///
/// # Example
///
/// ```ignore
/// let server = Server::default()
///     .with_protocol("moq-03")
///     .with_protocol("moq-04");
///
/// let session = server.accept(socket).await?;
/// ```
#[derive(Default, Clone)]
pub struct Server {
    protocols: Vec<String>,
}

impl Server {
    /// Add a supported application-level subprotocol for negotiation.
    ///
    /// The protocol will be prefixed with `webtransport:` on the wire.
    pub fn with_protocol(mut self, protocol: &str) -> Self {
        self.protocols.push(protocol.to_string());
        self
    }

    /// Add multiple supported application-level subprotocols for negotiation.
    ///
    /// Each protocol will be prefixed with `webtransport:` on the wire.
    pub fn with_protocols(mut self, protocols: &[&str]) -> Self {
        self.protocols
            .extend(protocols.iter().map(|s| s.to_string()));
        self
    }

    /// Accept a WebSocket connection, negotiating the subprotocol.
    ///
    /// The first client-requested protocol that matches one of the server's
    /// configured protocols will be selected.
    pub async fn accept<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        &self,
        socket: T,
    ) -> Result<Session, Error> {
        use std::sync::Mutex;

        let negotiated = Arc::new(Mutex::new(None::<String>));
        let negotiated_clone = negotiated.clone();
        let supported = self.protocols.clone();

        let callback = move |req: &server::Request,
                             mut response: server::Response|
              -> Result<server::Response, server::ErrorResponse> {
            let header_protocols: Vec<&str> = req
                .headers()
                .get(http::header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();

            if !header_protocols.iter().any(|p| *p == ALPN) {
                return Err(http::Response::builder()
                    .status(http::StatusCode::BAD_REQUEST)
                    .body(Some("'webtransport' protocol required".to_string()))
                    .unwrap());
            }

            // Match client-requested prefixed protocols against our supported list.
            let selected = header_protocols
                .iter()
                .filter_map(|p| p.strip_prefix(PREFIX))
                .find(|p| supported.iter().any(|s| s == p))
                .map(|p| p.to_string());

            let response_value = match &selected {
                Some(proto) => format!("{}, {PREFIX}{proto}", ALPN),
                None => ALPN.to_string(),
            };

            response.headers_mut().insert(
                http::header::SEC_WEBSOCKET_PROTOCOL,
                http::HeaderValue::from_str(&response_value).unwrap(),
            );

            *negotiated_clone.lock().unwrap() = selected;

            Ok(response)
        };

        let ws = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, None).await?;
        let protocol = negotiated.lock().unwrap().take();
        Ok(Session::new(ws, true, protocol))
    }
}
