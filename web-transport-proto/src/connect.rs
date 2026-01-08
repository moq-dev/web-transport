use std::{str::FromStr, sync::Arc};

use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

use super::{qpack, Frame, VarInt};

use thiserror::Error;

mod protocol_negotiation {
    //! WebTransport sub-protocol negotiation,
    //!
    //! according to [draft 14](https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-14.html#section-3.3)

    /// The header name for the available protocols, sent within the WebTransport Connect request.
    pub const AVAILABLE_NAME: &str = "wt-available-protocols";
    /// The header name for the selected protocol, sent within the WebTransport Connect response.
    pub const SELECTED_NAME: &str = "wt-protocol";
}

// Errors that can occur during the connect request.
#[derive(Error, Debug, Clone)]
pub enum ConnectError {
    #[error("unexpected end of input")]
    UnexpectedEnd,

    #[error("qpack error")]
    QpackError(#[from] qpack::DecodeError),

    #[error("unexpected frame {0:?}")]
    UnexpectedFrame(Frame),

    #[error("invalid method")]
    InvalidMethod,

    #[error("invalid url")]
    InvalidUrl(#[from] url::ParseError),

    #[error("invalid status")]
    InvalidStatus,

    #[error("expected 200, got: {0:?}")]
    WrongStatus(Option<http::StatusCode>),

    #[error("expected connect, got: {0:?}")]
    WrongMethod(Option<http::method::Method>),

    #[error("expected https, got: {0:?}")]
    WrongScheme(Option<String>),

    #[error("expected authority header")]
    WrongAuthority,

    #[error("expected webtransport, got: {0:?}")]
    WrongProtocol(Option<String>),

    #[error("expected path header")]
    WrongPath,

    #[error("non-200 status: {0:?}")]
    ErrorStatus(http::StatusCode),

    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),
}

impl From<std::io::Error> for ConnectError {
    fn from(err: std::io::Error) -> Self {
        ConnectError::Io(Arc::new(err))
    }
}

#[derive(Debug)]
pub struct ConnectRequest {
    /// The URL to connect to.
    pub url: Url,
    /// The subprotocols requested (if any).
    pub subprotocols: Vec<String>,
}

impl ConnectRequest {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, ConnectError> {
        let (typ, mut data) = Frame::read(buf).map_err(|_| ConnectError::UnexpectedEnd)?;
        if typ != Frame::HEADERS {
            return Err(ConnectError::UnexpectedFrame(typ));
        }

        // We no longer return UnexpectedEnd because we know the buffer should be large enough.

        let headers = qpack::Headers::decode(&mut data)?;

        let scheme = match headers.get(":scheme") {
            Some("https") => "https",
            Some(scheme) => Err(ConnectError::WrongScheme(Some(scheme.to_string())))?,
            None => return Err(ConnectError::WrongScheme(None)),
        };

        let authority = headers
            .get(":authority")
            .ok_or(ConnectError::WrongAuthority)?;

        let path_and_query = headers.get(":path").ok_or(ConnectError::WrongPath)?;

        let method = headers.get(":method");
        match method
            .map(|method| method.try_into().map_err(|_| ConnectError::InvalidMethod))
            .transpose()?
        {
            Some(http::Method::CONNECT) => (),
            o => return Err(ConnectError::WrongMethod(o)),
        };

        let protocol = headers.get(":protocol");
        if protocol != Some("webtransport") {
            return Err(ConnectError::WrongProtocol(protocol.map(|s| s.to_string())));
        }

        let subprotocols =
            if let Some(subprotocols) = headers.get(protocol_negotiation::AVAILABLE_NAME) {
                subprotocols
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

        let url = Url::parse(&format!("{scheme}://{authority}{path_and_query}"))?;

        Ok(Self { url, subprotocols })
    }

    pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Self, ConnectError> {
        let mut buf = Vec::new();
        loop {
            if stream.read_buf(&mut buf).await? == 0 {
                return Err(ConnectError::UnexpectedEnd);
            }

            let mut limit = std::io::Cursor::new(&buf);
            match Self::decode(&mut limit) {
                Ok(request) => return Ok(request),
                Err(ConnectError::UnexpectedEnd) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        let mut headers = qpack::Headers::default();
        headers.set(":method", "CONNECT");
        headers.set(":scheme", self.url.scheme());
        headers.set(":authority", self.url.authority());
        let path_and_query = match self.url.query() {
            Some(query) => format!("{}?{}", self.url.path(), query),
            None => self.url.path().to_string(),
        };
        headers.set(":path", &path_and_query);
        headers.set(":protocol", "webtransport");
        if !self.subprotocols.is_empty() {
            headers.set(
                protocol_negotiation::AVAILABLE_NAME,
                &self.subprotocols.join(", "),
            );
        }

        // Use a temporary buffer so we can compute the size.
        let mut tmp = Vec::new();
        headers.encode(&mut tmp);
        let size = VarInt::from_u32(tmp.len() as u32);

        Frame::HEADERS.encode(buf);
        size.encode(buf);
        buf.put_slice(&tmp);
    }

    pub async fn write<S: AsyncWrite + Unpin>(&self, stream: &mut S) -> Result<(), ConnectError> {
        let mut buf = BytesMut::new();
        self.encode(&mut buf);
        stream.write_all_buf(&mut buf).await?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct ConnectResponse {
    /// The status code of the response.
    pub status: http::status::StatusCode,
    /// The subprotocol selected by the server, if any
    pub subprotocol: Option<String>,
}

impl ConnectResponse {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, ConnectError> {
        let (typ, mut data) = Frame::read(buf).map_err(|_| ConnectError::UnexpectedEnd)?;
        if typ != Frame::HEADERS {
            return Err(ConnectError::UnexpectedFrame(typ));
        }

        let headers = qpack::Headers::decode(&mut data)?;

        let status = match headers
            .get(":status")
            .map(|status| {
                http::StatusCode::from_str(status).map_err(|_| ConnectError::InvalidStatus)
            })
            .transpose()?
        {
            Some(status) if status.is_success() => status,
            o => return Err(ConnectError::WrongStatus(o)),
        };

        let subprotocol = headers
            .get(protocol_negotiation::SELECTED_NAME)
            .map(|s| s.to_string());

        Ok(Self {
            status,
            subprotocol,
        })
    }

    pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Self, ConnectError> {
        let mut buf = Vec::new();
        loop {
            if stream.read_buf(&mut buf).await? == 0 {
                return Err(ConnectError::UnexpectedEnd);
            }

            let mut limit = std::io::Cursor::new(&buf);
            match Self::decode(&mut limit) {
                Ok(response) => return Ok(response),
                Err(ConnectError::UnexpectedEnd) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        let mut headers = qpack::Headers::default();
        headers.set(":status", self.status.as_str());
        headers.set("sec-webtransport-http3-draft", "draft02");
        if let Some(subprotocol) = &self.subprotocol {
            headers.set(protocol_negotiation::SELECTED_NAME, subprotocol);
        }

        // Use a temporary buffer so we can compute the size.
        let mut tmp = Vec::new();
        headers.encode(&mut tmp);
        let size = VarInt::from_u32(tmp.len() as u32);

        Frame::HEADERS.encode(buf);
        size.encode(buf);
        buf.put_slice(&tmp);
    }

    pub async fn write<S: AsyncWrite + Unpin>(&self, stream: &mut S) -> Result<(), ConnectError> {
        let mut buf = BytesMut::new();
        self.encode(&mut buf);
        stream.write_all_buf(&mut buf).await?;
        Ok(())
    }
}
