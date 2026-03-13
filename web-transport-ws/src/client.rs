/// A WebTransport client that connects over WebSocket.
///
/// # Deprecated
///
/// Use [`qmux::ws::Client`] instead.
#[deprecated(note = "use qmux::ws::Client instead")]
#[derive(Default, Clone)]
pub struct Client {
    inner: qmux::ws::Client,
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

    /// Set the WebSocket configuration (e.g. max message/frame sizes).
    pub fn with_config(mut self, config: qmux::tungstenite::protocol::WebSocketConfig) -> Self {
        self.inner = self.inner.with_config(config);
        self
    }

    /// Set the TLS connector for secure WebSocket connections.
    #[cfg(any(feature = "rustls-tls-native-roots", feature = "rustls-tls-webpki-roots"))]
    pub fn with_connector(mut self, connector: qmux::tokio_tungstenite::Connector) -> Self {
        self.inner = self.inner.with_connector(connector);
        self
    }

    /// Connect to a WebSocket server, negotiating the configured subprotocols.
    pub async fn connect(&self, url: &str) -> Result<qmux::Session, qmux::Error> {
        self.inner.connect(url).await
    }
}
