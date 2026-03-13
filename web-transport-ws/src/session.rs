use qmux::tungstenite;

/// A WebTransport session over WebSocket.
///
/// # Deprecated
///
/// Use [`qmux::Session`] with [`qmux::ws::accept`] / [`qmux::ws::connect`] instead.
#[deprecated(note = "use qmux::Session with qmux::ws::accept/connect instead")]
#[derive(Clone)]
pub struct Session(qmux::Session);

#[allow(deprecated)]
impl Session {
    /// Wrap a pre-upgraded WebSocket as a server-side session.
    pub fn accept<T>(ws: T, protocol: Option<String>) -> Self
    where
        T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
            + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        Self(qmux::ws::accept(ws, protocol.as_deref()))
    }

    /// Wrap a pre-upgraded WebSocket as a client-side session.
    pub fn connect<T>(ws: T, protocol: Option<String>) -> Self
    where
        T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
            + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        Self(qmux::ws::connect(ws, protocol.as_deref()))
    }

    /// Get the inner qmux::Session.
    pub fn into_inner(self) -> qmux::Session {
        self.0
    }
}

#[allow(deprecated)]
impl From<Session> for qmux::Session {
    fn from(s: Session) -> Self {
        s.0
    }
}

#[allow(deprecated)]
impl From<qmux::Session> for Session {
    fn from(s: qmux::Session) -> Self {
        Self(s)
    }
}

#[allow(deprecated)]
impl web_transport_trait::Session for Session {
    type SendStream = qmux::SendStream;
    type RecvStream = qmux::RecvStream;
    type Error = qmux::Error;

    async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
        self.0.accept_uni().await
    }

    async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        self.0.accept_bi().await
    }

    async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
        self.0.open_uni().await
    }

    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        self.0.open_bi().await
    }

    fn close(&self, code: u32, reason: &str) {
        self.0.close(code, reason)
    }

    async fn closed(&self) -> Self::Error {
        self.0.closed().await
    }

    fn protocol(&self) -> Option<&str> {
        self.0.protocol()
    }

    fn send_datagram(&self, payload: bytes::Bytes) -> Result<(), Self::Error> {
        self.0.send_datagram(payload)
    }

    async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
        self.0.recv_datagram().await
    }

    fn max_datagram_size(&self) -> usize {
        self.0.max_datagram_size()
    }
}
