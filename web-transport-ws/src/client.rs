use crate::tungstenite;

/// A WebTransport client that connects over WebSocket.
///
/// # Deprecated
///
/// Use [`qmux::Client`] instead, which supports TCP, TLS, and WebSocket.
#[deprecated(note = "use qmux::Client instead")]
#[derive(Default, Clone)]
pub struct Client {
    inner: qmux::Client,
    config: Option<tungstenite::protocol::WebSocketConfig>,
    #[cfg(any(
        feature = "rustls-tls-native-roots",
        feature = "rustls-tls-webpki-roots"
    ))]
    connector: Option<tokio_tungstenite::Connector>,
}

#[allow(deprecated)]
impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a supported application-level subprotocol for negotiation.
    pub fn with_protocol(mut self, protocol: &str) -> Self {
        self.inner = self.inner.with_protocol(protocol);
        self
    }

    /// Add multiple supported application-level subprotocols for negotiation.
    pub fn with_protocols(mut self, protocols: &[&str]) -> Self {
        self.inner = self.inner.with_protocols(protocols);
        self
    }

    /// Set custom WebSocket configuration (max message size, frame size, etc).
    ///
    /// Note: This option is not forwarded to the underlying `qmux` client.
    /// Migrate to `qmux::Client` for full control.
    pub fn with_config(mut self, config: tungstenite::protocol::WebSocketConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set a custom TLS connector for secure connections.
    ///
    /// Note: This option is not forwarded to the underlying `qmux` client.
    /// Migrate to `qmux::Client` for full control.
    #[cfg(any(
        feature = "rustls-tls-native-roots",
        feature = "rustls-tls-webpki-roots"
    ))]
    pub fn with_connector(mut self, connector: tokio_tungstenite::Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// Connect to a WebSocket server, negotiating the configured subprotocols.
    pub async fn connect(&self, url: &str) -> Result<qmux::Session, qmux::Error> {
        self.inner.connect_ws(url).await
    }
}
