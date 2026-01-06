use http::HeaderMap;
use web_transport_proto::{ConnectRequest, ConnectResponse, VarInt};

use thiserror::Error;
use url::Url;

#[derive(Error, Debug)]
pub enum ConnectError {
    #[error("quic stream was closed early")]
    UnexpectedEnd,

    #[error("already responded")]
    AlreadyResponded,

    #[error("protocol error: {0}")]
    ProtoError(#[from] web_transport_proto::ConnectError),

    #[error("connection error")]
    ConnectionError(#[from] quinn::ConnectionError),

    #[error("read error")]
    ReadError(#[from] quinn::ReadError),

    #[error("write error")]
    WriteError(#[from] quinn::WriteError),

    #[error("http error status: {0}")]
    ErrorStatus(http::StatusCode),
}

pub struct Connect {
    // The request that was sent by the client.
    pub request: ConnectRequest,
    // The response that was sent by the server.
    pub response: Option<ConnectResponse>,

    // A reference to the send/recv stream, so we don't close it until dropped.
    send: quinn::SendStream,

    #[allow(dead_code)]
    recv: quinn::RecvStream,
}

impl Connect {
    pub async fn accept(conn: &quinn::Connection) -> Result<Self, ConnectError> {
        // Accept the stream that will be used to send the HTTP CONNECT request.
        // If they try to send any other type of HTTP request, we will error out.
        let (send, mut recv) = conn.accept_bi().await?;
        log::debug!("[{}] Opening to read", conn.remote_address());

        let request = web_transport_proto::ConnectRequest::read(&mut recv).await?;
        log::debug!(
            "[{}] received CONNECT request: {:?}",
            conn.remote_address(),
            request.url
        );

        // The request was successfully decoded, so we can send a response.
        Ok(Self {
            request,
            response: None,
            send,
            recv,
        })
    }

    // Called by the server to send a response to the client.
    pub async fn respond(&mut self, status: http::StatusCode) -> Result<(), ConnectError> {
        self.respond_with_headers(status, Default::default()).await
    }

    pub async fn respond_with_headers(
        &mut self,
        status: http::StatusCode,
        headers: HeaderMap,
    ) -> Result<(), ConnectError> {
        if self.response.is_some() {
            return Err(ConnectError::AlreadyResponded); // don't sent the CONNECT twice
        }
        let resp = ConnectResponse { status, headers };

        log::debug!("sending CONNECT response: {resp:?}");
        resp.write(&mut self.send).await?;
        self.response = Some(resp);

        Ok(())
    }

    pub async fn open_with_headers(
        conn: &quinn::Connection,
        url: Url,
        headers: HeaderMap,
    ) -> Result<Self, ConnectError> {
        // Create a new stream that will be used to send the CONNECT frame.
        let (mut send, mut recv) = conn.open_bi().await?;

        // Create a new CONNECT request that we'll send using HTTP/3
        let request = ConnectRequest { url, headers };

        log::debug!("sending CONNECT request: {request:?}");
        request.write(&mut send).await?;

        let response = web_transport_proto::ConnectResponse::read(&mut recv).await?;
        log::debug!("received CONNECT response: {response:?}");

        // Throw an error if we didn't get a 200 OK.
        if response.status != http::StatusCode::OK {
            return Err(ConnectError::ErrorStatus(response.status));
        }

        Ok(Self {
            request,
            response: Some(response),
            send,
            recv,
        })
    }

    // The session ID is the stream ID of the CONNECT request.
    pub fn session_id(&self) -> VarInt {
        // We gotta convert from the Quinn VarInt to the (forked) WebTransport VarInt.
        // We don't use the quinn::VarInt because that would mean a quinn dependency in web-transport-proto
        let stream_id = quinn::VarInt::from(self.send.id());
        VarInt::try_from(stream_id.into_inner()).unwrap()
    }

    // The URL in the CONNECT request.
    pub fn url(&self) -> &Url {
        &self.request.url
    }

    pub(super) fn into_inner(self) -> (quinn::SendStream, quinn::RecvStream) {
        (self.send, self.recv)
    }
}
