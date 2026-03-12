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

    /// Connect to a WebSocket server, negotiating the configured subprotocols.
    pub async fn connect(&self, url: &str) -> Result<qmux::Session, qmux::Error> {
        self.inner.connect(url).await
    }
}
