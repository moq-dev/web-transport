use crate::proto::{ConnectRequest, ConnectResponse, VarInt};

use thiserror::Error;

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
pub struct Connecting {
    // The request that was sent by the client.
    pub request: ConnectRequest,

    // A reference to the send/recv stream, so we don't close it until dropped.
    send: ez::SendStream,

    #[allow(dead_code)]
    recv: ez::RecvStream,
}

impl Connecting {
    /// Accept an HTTP/3 CONNECT request from the client.
    ///
    /// This is called by the server to receive the CONNECT request.
    pub async fn accept(conn: &ez::Connection) -> Result<Self, ConnectError> {
        // Accept the stream that will be used to send the HTTP CONNECT request.
        // If they try to send any other type of HTTP request, we will error out.
        let (send, mut recv) = conn.accept_bi().await?;

        let request = web_transport_proto::ConnectRequest::read(&mut recv).await?;
        tracing::debug!(?request, "received CONNECT");

        // The request was successfully decoded, so we can send a response.
        Ok(Self {
            request,
            send,
            recv,
        })
    }

    /// Send an HTTP/3 CONNECT response to the client.
    ///
    /// This is called by the server to accept or reject the connection.
    pub async fn ok(
        mut self,
        response: impl Into<ConnectResponse>,
    ) -> Result<Connected, ConnectError> {
        let response = response.into();

        tracing::debug!(?response, "sending CONNECT");
        response.write(&mut self.send).await?;

        Ok(Connected {
            request: self.request,
            response,
            send: self.send,
            recv: self.recv,
        })
    }

    pub async fn close(self, status: http::StatusCode) -> Result<(), ConnectError> {
        let mut connect = self.ok(status).await?;
        connect.send.finish()?;
        Ok(())
    }
}

impl core::ops::Deref for Connecting {
    type Target = ConnectRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

pub struct Connected {
    // The request that was sent by the client.
    pub request: ConnectRequest,

    // The response sent by the server.
    pub response: ConnectResponse,

    // A reference to the send/recv stream, so we don't close it until dropped.
    pub(crate) send: ez::SendStream,
    pub(crate) recv: ez::RecvStream,
}

impl Connected {
    /// Send an HTTP/3 CONNECT request to the server and wait for the response.
    ///
    /// This is called by the client to initiate a WebTransport session.
    pub async fn open(
        conn: &ez::Connection,
        request: impl Into<ConnectRequest>,
    ) -> Result<Self, ConnectError> {
        tracing::debug!("opening bi");

        // Create a new stream that will be used to send the CONNECT frame.
        let (mut send, mut recv) = conn.open_bi().await?;

        // Create a new CONNECT request that we'll send using HTTP/3
        let request = request.into();

        tracing::debug!(?request, "sending CONNECT");
        request.write(&mut send).await?;

        let response = web_transport_proto::ConnectResponse::read(&mut recv).await?;
        tracing::debug!(?response, "received CONNECT");

        // Throw an error if we didn't get a 200 OK.
        if response.status != http::StatusCode::OK {
            return Err(ConnectError::Status(response.status));
        }

        Ok(Self {
            request,
            response,
            send,
            recv,
        })
    }

    // The session ID is the stream ID of the CONNECT request.
    pub fn session_id(&self) -> VarInt {
        VarInt::try_from(u64::from(self.send.id())).unwrap()
    }
}
