use crate::{tungstenite, Session, ALPN};
use tungstenite::{client::IntoClientRequest, http};

use crate::Error;

/// The prefix required for all application subprotocols over this WebSocket compatibility layer.
const PREFIX: &str = "webtransport:";

/// A WebTransport client that connects over WebSocket.
///
/// # Example
///
/// ```ignore
/// let session = Client::new()
///     .with_protocol("moq-03")
///     .with_protocol("moq-04")
///     .connect("ws://localhost:4443")
///     .await?;
/// ```
#[derive(Default, Clone)]
pub struct Client {
    protocols: Vec<String>,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Connect to a WebSocket server, negotiating the configured subprotocols.
    pub async fn connect(&self, url: &str) -> Result<Session, Error> {
        let mut request = url.into_client_request()?;

        let protocol_value = if self.protocols.is_empty() {
            ALPN.to_string()
        } else {
            let prefixed: Vec<String> = self
                .protocols
                .iter()
                .map(|p| format!("{PREFIX}{p}"))
                .collect();
            format!("{}, {}", ALPN, prefixed.join(", "))
        };

        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            http::HeaderValue::from_str(&protocol_value).unwrap(),
        );

        let (ws_stream, response) = tokio_tungstenite::connect_async(request).await?;

        let negotiated = response
            .headers()
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| {
                h.split(',')
                    .map(|p| p.trim())
                    .find_map(|p| p.strip_prefix(PREFIX))
                    .map(|p| p.to_string())
            });

        Ok(Session::new(ws_stream, false, negotiated))
    }
}
