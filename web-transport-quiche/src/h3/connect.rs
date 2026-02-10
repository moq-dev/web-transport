use crate::proto::{ConnectRequest, ConnectResponse, VarInt};

use thiserror::Error;
use url::Url;

use crate::ez;

/// An error returned when exchanging the HTTP/3 CONNECT handshake.
#[derive(Error, Debug, Clone)]
pub enum ConnectError {
    #[error("quic stream was closed early")]
    UnexpectedEnd,

    #[error("protocol error: {0}")]
    Proto(#[from] web_transport_proto::ConnectError),

    #[error("connection error")]
    Connection(#[from] ez::ConnectionError),

    #[error("stream error")]
    Stream(#[from] ez::StreamError),

    #[error("http error status: {0}")]
    Status(http::StatusCode),
}

/// An HTTP/3 CONNECT request/response for establishing a WebTransport session.
pub struct Connect {
    // The request that was sent by the client.
    request: ConnectRequest,

    // A reference to the send/recv stream, so we don't close it until dropped.
    send: ez::SendStream,

    #[allow(dead_code)]
    recv: ez::RecvStream,

    // Leftover bytes from reading the CONNECT request/response.
    // These may contain capsule data that arrived with the HEADERS frame.
    buf: Vec<u8>,
}

impl Connect {
    /// Accept an HTTP/3 CONNECT request from the client.
    ///
    /// This is called by the server to receive the CONNECT request.
    pub async fn accept(conn: &ez::Connection) -> Result<Self, ConnectError> {
        let (send, mut recv) = conn.accept_bi().await?;

        let mut buf = Vec::new();
        let request = loop {
            if recv.read_buf(&mut buf).await?.is_none() {
                return Err(ConnectError::UnexpectedEnd);
            }

            let mut cursor = std::io::Cursor::new(buf.as_slice());
            match ConnectRequest::decode(&mut cursor) {
                Ok(request) => {
                    let consumed = cursor.position() as usize;
                    buf.drain(..consumed);
                    break request;
                }
                Err(web_transport_proto::ConnectError::UnexpectedEnd) => continue,
                Err(e) => return Err(e.into()),
            }
        };

        tracing::debug!(?request, leftover = buf.len(), "received CONNECT");

        Ok(Self {
            request,
            send,
            recv,
            buf,
        })
    }

    /// Send an HTTP/3 CONNECT response to the client.
    ///
    /// This is called by the server to accept or reject the connection.
    pub async fn respond(
        &mut self,
        response: impl Into<ConnectResponse>,
    ) -> Result<(), ConnectError> {
        let response = response.into();
        tracing::debug!(?response, "sending CONNECT");
        response.write(&mut self.send).await?;

        Ok(())
    }

    /// Send an HTTP/3 CONNECT request to the server and wait for the response.
    ///
    /// This is called by the client to initiate a WebTransport session.
    pub async fn open(
        conn: &ez::Connection,
        request: impl Into<ConnectRequest>,
    ) -> Result<Self, ConnectError> {
        tracing::debug!("opening bi");

        let (mut send, mut recv) = conn.open_bi().await?;

        let request = request.into();

        tracing::debug!(?request, "sending CONNECT");
        request.write(&mut send).await?;

        let mut buf = Vec::new();
        let response = loop {
            if recv.read_buf(&mut buf).await?.is_none() {
                return Err(ConnectError::UnexpectedEnd);
            }

            let mut cursor = std::io::Cursor::new(buf.as_slice());
            match ConnectResponse::decode(&mut cursor) {
                Ok(response) => {
                    let consumed = cursor.position() as usize;
                    buf.drain(..consumed);
                    break response;
                }
                Err(web_transport_proto::ConnectError::UnexpectedEnd) => continue,
                Err(e) => return Err(e.into()),
            }
        };

        tracing::debug!(?response, leftover = buf.len(), "received CONNECT");

        if response.status != http::StatusCode::OK {
            return Err(ConnectError::Status(response.status));
        }

        Ok(Self {
            request,
            send,
            recv,
            buf,
        })
    }

    // The session ID is the stream ID of the CONNECT request.
    pub fn session_id(&self) -> VarInt {
        VarInt::try_from(u64::from(self.send.id())).unwrap()
    }

    // The URL in the CONNECT request.
    pub fn url(&self) -> &Url {
        &self.request.url
    }

    /// Returns the inner streams and any leftover bytes from reading the CONNECT handshake.
    pub fn into_inner(self) -> (ez::SendStream, ez::RecvStream, Vec<u8>) {
        (self.send, self.recv, self.buf)
    }
}
