use std::ops::Deref;

use iroh::endpoint::{self, Connection, RecvStream, SendStream};
use n0_error::stack_error;
use web_transport_proto::{ConnectRequest, ConnectResponse, VarInt};

/// An error during the HTTP/3 CONNECT handshake.
#[derive(Clone)]
#[stack_error(derive, from_sources)]
pub enum ConnectError {
    #[error("quic stream was closed early")]
    UnexpectedEnd,

    #[error("protocol error")]
    ProtoError(#[error(source, from, std_err)] web_transport_proto::ConnectError),

    #[error("connection error")]
    ConnectionError(#[error(source, from, std_err)] endpoint::ConnectionError),

    #[error("read error")]
    ReadError(#[error(source, from, std_err)] endpoint::ReadError),

    #[error("write error")]
    WriteError(#[error(source, from, std_err)] endpoint::WriteError),

    #[error("http error status: {_0}")]
    ErrorStatus(http::StatusCode),

    #[error("server returned protocol not in request: {_0}")]
    ProtocolMismatch(String),
}

/// An in-progress HTTP/3 CONNECT handshake, awaiting a response.
#[derive(Debug)]
pub struct Connecting {
    // The request that was sent by the client.
    request: ConnectRequest,

    // A reference to the send/recv stream, so we don't close it until dropped.
    send: SendStream,

    #[allow(dead_code)]
    recv: RecvStream,
}

impl Connecting {
    /// Accepts an incoming HTTP/3 CONNECT request from the client.
    pub async fn accept(conn: &Connection) -> Result<Self, ConnectError> {
        // Accept the stream that will be used to send the HTTP CONNECT request.
        // If they try to send any other type of HTTP request, we will error out.
        let (send, mut recv) = conn.accept_bi().await?;

        let request = web_transport_proto::ConnectRequest::read(&mut recv).await?;
        tracing::debug!("received CONNECT request: {request:?}");

        // The request was successfully decoded, so we can send a response.
        Ok(Self {
            request,
            send,
            recv,
        })
    }

    /// Sends a response to the client and establishes the session.
    pub async fn respond(
        mut self,
        response: impl Into<ConnectResponse>,
    ) -> Result<Connected, ConnectError> {
        let response = response.into();

        // Validate that our protocol was in the client's request.
        if let Some(protocol) = &response.protocol
            && !self.request.protocols.contains(protocol)
        {
            return Err(ConnectError::ProtocolMismatch(protocol.clone()));
        }

        tracing::debug!(?response, "sending CONNECT response");
        response.write(&mut self.send).await?;

        Ok(Connected {
            request: self.request,
            response,
            send: self.send,
            recv: self.recv,
        })
    }

    /// Rejects the CONNECT request with the given status code.
    pub async fn reject(self, status: http::StatusCode) -> Result<(), ConnectError> {
        let mut connect = self.respond(status).await?;
        connect.send.finish().ok();
        Ok(())
    }
}

impl Deref for Connecting {
    type Target = ConnectRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

/// An established HTTP/3 CONNECT session with both request and response.
#[derive(Debug)]
pub struct Connected {
    /// The request sent by the client.
    pub request: ConnectRequest,

    /// The response sent by the server.
    pub response: ConnectResponse,

    // A reference to the send/recv stream, so we don't close it until dropped.
    pub(crate) send: SendStream,
    pub(crate) recv: RecvStream,
}

impl Connected {
    /// Open a new WebTransport session on the given connection for the given URL.
    ///
    /// You may add any number of subprotocols allowing the server to select from.
    /// If the list is empty the field will be omitted in the request header.
    pub async fn open(
        conn: &Connection,
        request: impl Into<ConnectRequest>,
    ) -> Result<Self, ConnectError> {
        let request = request.into();

        // Create a new stream that will be used to send the CONNECT frame.
        let (mut send, mut recv) = conn.open_bi().await?;

        tracing::debug!(?request, "sending CONNECT request");
        request.write(&mut send).await?;

        let response = web_transport_proto::ConnectResponse::read(&mut recv).await?;
        tracing::debug!(?response, "received CONNECT response");

        // Throw an error if we didn't get a 200 OK.
        if response.status != http::StatusCode::OK {
            return Err(ConnectError::ErrorStatus(response.status));
        }

        // Validate that the server's protocol was in our request.
        if let Some(protocol) = &response.protocol
            && !request.protocols.contains(protocol)
        {
            return Err(ConnectError::ProtocolMismatch(protocol.clone()));
        }

        Ok(Self {
            request,
            response,
            send,
            recv,
        })
    }

    /// Returns the session ID, which is the stream ID of the CONNECT request.
    pub fn session_id(&self) -> VarInt {
        // We gotta convert from the Quinn VarInt to the (forked) WebTransport VarInt.
        // We don't use the iroh::endpoint::VarInt because that would mean a iroh::endpoint dependency in web-transport-proto
        let stream_id = endpoint::VarInt::from(self.send.id());
        VarInt::try_from(stream_id.into_inner()).unwrap()
    }

    // Keep reading from the control stream until it's closed.
    pub(crate) async fn run_closed(&mut self) -> (u32, String) {
        loop {
            match web_transport_proto::Capsule::read(&mut self.recv).await {
                Ok(Some(web_transport_proto::Capsule::CloseWebTransportSession {
                    code,
                    reason,
                })) => {
                    return (code, reason);
                }
                Ok(Some(web_transport_proto::Capsule::Grease { .. })) => {}
                Ok(Some(web_transport_proto::Capsule::Unknown { typ, payload })) => {
                    tracing::warn!(%typ, size = payload.len(), "unknown capsule");
                }
                Ok(None) => {
                    return (0, "stream closed".to_string());
                }
                Err(_) => {
                    return (1, "capsule error".to_string());
                }
            }
        }
    }
}
