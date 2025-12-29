/// Unified WebTransport session that can be either Quinn (QUIC) or WebSocket
#[derive(Clone)]
pub enum WebTransportSessionAny {
    Quinn(web_transport_quinn::Session),
    WebSocket(web_transport_ws::Session),
}

impl From<web_transport_quinn::Session> for WebTransportSessionAny {
    fn from(session: web_transport_quinn::Session) -> Self {
        WebTransportSessionAny::Quinn(session)
    }
}

impl From<web_transport_ws::Session> for WebTransportSessionAny {
    fn from(session: web_transport_ws::Session) -> Self {
        WebTransportSessionAny::WebSocket(session)
    }
}

impl WebTransportSessionAny {
    /// Returns the underlying web_transport_quinn::Session if this is a Quinn transport,
    /// or panics if it's a WebSocket transport.
    ///
    /// For backward compatibility with code expecting web_transport_quinn::Session.
    /// New code should handle both variants properly.
    pub fn into_quic(self) -> web_transport_quinn::Session {
        match self {
            WebTransportSessionAny::Quinn(session) => session,
            WebTransportSessionAny::WebSocket(_) => {
                panic!("Expected Quinn session but got WebSocket")
            }
        }
    }
}

// Unified error type that can hold either transport's error
#[derive(Debug)]
pub enum WebTransportErrorAny {
    QuinnSession(web_transport_quinn::SessionError),
    QuinnWrite(web_transport_quinn::WriteError),
    QuinnRead(web_transport_quinn::ReadError),
    WebSocket(web_transport_ws::Error),
}

impl std::fmt::Display for WebTransportErrorAny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebTransportErrorAny::QuinnSession(e) => write!(f, "Quinn session error: {}", e),
            WebTransportErrorAny::QuinnWrite(e) => write!(f, "Quinn write error: {}", e),
            WebTransportErrorAny::QuinnRead(e) => write!(f, "Quinn read error: {}", e),
            WebTransportErrorAny::WebSocket(e) => write!(f, "WebSocket error: {}", e),
        }
    }
}

impl std::error::Error for WebTransportErrorAny {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WebTransportErrorAny::QuinnSession(e) => Some(e),
            WebTransportErrorAny::QuinnWrite(e) => Some(e),
            WebTransportErrorAny::QuinnRead(e) => Some(e),
            WebTransportErrorAny::WebSocket(e) => Some(e),
        }
    }
}

impl web_transport_trait::Error for WebTransportErrorAny {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            WebTransportErrorAny::QuinnSession(e) => e.session_error(),
            WebTransportErrorAny::QuinnWrite(e) => e.session_error(),
            WebTransportErrorAny::QuinnRead(e) => e.session_error(),
            WebTransportErrorAny::WebSocket(e) => e.session_error(),
        }
    }
}

impl From<web_transport_quinn::SessionError> for WebTransportErrorAny {
    fn from(error: web_transport_quinn::SessionError) -> Self {
        WebTransportErrorAny::QuinnSession(error)
    }
}

impl From<web_transport_quinn::WriteError> for WebTransportErrorAny {
    fn from(error: web_transport_quinn::WriteError) -> Self {
        WebTransportErrorAny::QuinnWrite(error)
    }
}

impl From<web_transport_quinn::ReadError> for WebTransportErrorAny {
    fn from(error: web_transport_quinn::ReadError) -> Self {
        WebTransportErrorAny::QuinnRead(error)
    }
}

impl From<web_transport_ws::Error> for WebTransportErrorAny {
    fn from(error: web_transport_ws::Error) -> Self {
        WebTransportErrorAny::WebSocket(error)
    }
}

// Unified stream types
pub enum WebTransportSendStreamAny {
    Quinn(web_transport_quinn::SendStream),
    WebSocket(web_transport_ws::SendStream),
}

impl web_transport_trait::SendStream for WebTransportSendStreamAny {
    type Error = WebTransportErrorAny;

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        match self {
            WebTransportSendStreamAny::Quinn(s) => s.write(buf).await.map_err(Into::into),
            WebTransportSendStreamAny::WebSocket(s) => s.write(buf).await.map_err(Into::into),
        }
    }

    fn set_priority(&mut self, order: u8) {
        match self {
            WebTransportSendStreamAny::Quinn(s) => s.set_priority(order),
            WebTransportSendStreamAny::WebSocket(s) => s.set_priority(order),
        }
    }

    fn reset(&mut self, code: u32) {
        match self {
            WebTransportSendStreamAny::Quinn(s) => {
                let _ = s.reset(code);
            }
            WebTransportSendStreamAny::WebSocket(s) => s.reset(code),
        }
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        match self {
            WebTransportSendStreamAny::Quinn(s) => {
                web_transport_trait::SendStream::finish(s).map_err(Into::into)
            }
            WebTransportSendStreamAny::WebSocket(s) => s.finish().map_err(Into::into),
        }
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        match self {
            WebTransportSendStreamAny::Quinn(s) => s.closed().await.map_err(Into::into),
            WebTransportSendStreamAny::WebSocket(s) => s.closed().await.map_err(Into::into),
        }
    }
}

impl From<web_transport_quinn::SendStream> for WebTransportSendStreamAny {
    fn from(stream: web_transport_quinn::SendStream) -> Self {
        WebTransportSendStreamAny::Quinn(stream)
    }
}

impl From<web_transport_ws::SendStream> for WebTransportSendStreamAny {
    fn from(stream: web_transport_ws::SendStream) -> Self {
        WebTransportSendStreamAny::WebSocket(stream)
    }
}

pub enum WebTransportRecvStreamAny {
    Quinn(web_transport_quinn::RecvStream),
    WebSocket(web_transport_ws::RecvStream),
}

impl web_transport_trait::RecvStream for WebTransportRecvStreamAny {
    type Error = WebTransportErrorAny;

    async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        match self {
            WebTransportRecvStreamAny::Quinn(s) => s.read(dst).await.map_err(Into::into),
            WebTransportRecvStreamAny::WebSocket(s) => s.read(dst).await.map_err(Into::into),
        }
    }

    fn stop(&mut self, code: u32) {
        match self {
            WebTransportRecvStreamAny::Quinn(s) => s.stop(code).ok().unwrap_or_default(),
            WebTransportRecvStreamAny::WebSocket(s) => s.stop(code),
        }
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        match self {
            WebTransportRecvStreamAny::Quinn(s) => s.closed().await.map_err(Into::into),
            WebTransportRecvStreamAny::WebSocket(s) => s.closed().await.map_err(Into::into),
        }
    }
}

impl From<web_transport_quinn::RecvStream> for WebTransportRecvStreamAny {
    fn from(stream: web_transport_quinn::RecvStream) -> Self {
        WebTransportRecvStreamAny::Quinn(stream)
    }
}

impl From<web_transport_ws::RecvStream> for WebTransportRecvStreamAny {
    fn from(stream: web_transport_ws::RecvStream) -> Self {
        WebTransportRecvStreamAny::WebSocket(stream)
    }
}

impl web_transport_trait::Session for WebTransportSessionAny {
    type SendStream = WebTransportSendStreamAny;
    type RecvStream = WebTransportRecvStreamAny;
    type Error = WebTransportErrorAny;

    async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
        match self {
            WebTransportSessionAny::Quinn(s) => {
                s.accept_uni().await.map(Into::into).map_err(Into::into)
            }
            WebTransportSessionAny::WebSocket(s) => {
                s.accept_uni().await.map(Into::into).map_err(Into::into)
            }
        }
    }

    async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        match self {
            WebTransportSessionAny::Quinn(s) => s
                .accept_bi()
                .await
                .map(|(send, recv)| (send.into(), recv.into()))
                .map_err(Into::into),
            WebTransportSessionAny::WebSocket(s) => s
                .accept_bi()
                .await
                .map(|(send, recv)| (send.into(), recv.into()))
                .map_err(Into::into),
        }
    }

    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        match self {
            WebTransportSessionAny::Quinn(s) => s
                .open_bi()
                .await
                .map(|(send, recv)| (send.into(), recv.into()))
                .map_err(Into::into),
            WebTransportSessionAny::WebSocket(s) => s
                .open_bi()
                .await
                .map(|(send, recv)| (send.into(), recv.into()))
                .map_err(Into::into),
        }
    }

    async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
        match self {
            WebTransportSessionAny::Quinn(s) => {
                s.open_uni().await.map(Into::into).map_err(Into::into)
            }
            WebTransportSessionAny::WebSocket(s) => {
                s.open_uni().await.map(Into::into).map_err(Into::into)
            }
        }
    }

    fn send_datagram(&self, payload: bytes::Bytes) -> Result<(), Self::Error> {
        match self {
            WebTransportSessionAny::Quinn(s) => s.send_datagram(payload).map_err(Into::into),
            WebTransportSessionAny::WebSocket(s) => s.send_datagram(payload).map_err(Into::into),
        }
    }

    async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
        match self {
            WebTransportSessionAny::Quinn(s) => s.recv_datagram().await.map_err(Into::into),
            WebTransportSessionAny::WebSocket(s) => s.recv_datagram().await.map_err(Into::into),
        }
    }

    fn max_datagram_size(&self) -> usize {
        match self {
            WebTransportSessionAny::Quinn(s) => s.max_datagram_size(),
            WebTransportSessionAny::WebSocket(s) => s.max_datagram_size(),
        }
    }

    fn close(&self, code: u32, reason: &str) {
        match self {
            WebTransportSessionAny::Quinn(s) => {
                web_transport_trait::Session::close(s, code, reason)
            }
            WebTransportSessionAny::WebSocket(s) => s.close(code, reason),
        }
    }

    async fn closed(&self) -> Self::Error {
        match self {
            WebTransportSessionAny::Quinn(s) => s.closed().await.into(),
            WebTransportSessionAny::WebSocket(s) => s.closed().await.into(),
        }
    }
}
