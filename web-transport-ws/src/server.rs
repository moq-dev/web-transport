use tokio::io::{AsyncRead, AsyncWrite};

/// A WebTransport server that accepts WebSocket connections.
///
/// # Deprecated
///
/// Use [`qmux::ws::Server`] instead.
#[deprecated(note = "use qmux::ws::Server instead")]
#[derive(Default, Clone)]
pub struct Server {
    inner: qmux::ws::Server,
}

#[allow(deprecated)]
impl Server {
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

    /// Accept a WebSocket connection, negotiating the subprotocol.
    pub async fn accept<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        &self,
        socket: T,
    ) -> Result<qmux::Session, qmux::Error> {
        self.inner.accept(socket).await
    }
}
