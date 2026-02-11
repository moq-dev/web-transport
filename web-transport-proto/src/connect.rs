use std::{str::FromStr, sync::Arc};

use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

use super::{qpack, Frame, VarInt};

use thiserror::Error;

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

    #[error("invalid protocol header")]
    InvalidProtocol,

    #[error("structured field error: {0}")]
    StructuredFieldError(Arc<sfv::Error>),

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

impl From<sfv::Error> for ConnectError {
    fn from(err: sfv::Error) -> Self {
        ConnectError::StructuredFieldError(Arc::new(err))
    }
}

/// A CONNECT request to initiate a WebTransport session.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    /// The URL to connect to.
    pub url: Url,

    /// The subprotocols requested (if any).
    pub protocols: Vec<String>,
}

impl ConnectRequest {
    pub fn new(url: impl Into<Url>) -> Self {
        Self {
            url: url.into(),
            protocols: Vec::new(),
        }
    }

    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocols.push(protocol.into());
        self
    }

    pub fn with_protocols(
        mut self,
        protocols: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.protocols
            .extend(protocols.into_iter().map(|p| p.into()));
        self
    }

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

        let protocols = headers
            .get(protocol_negotiation::AVAILABLE_NAME)
            .map(protocol_negotiation::decode_list)
            .transpose()
            .map_err(|_| ConnectError::InvalidProtocol)?
            .unwrap_or_default();

        let url = Url::parse(&format!("{scheme}://{authority}{path_and_query}"))?;

        Ok(Self { url, protocols })
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

    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), ConnectError> {
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

        if !self.protocols.is_empty() {
            let encoded = protocol_negotiation::encode_list(&self.protocols)?;
            headers.set(protocol_negotiation::AVAILABLE_NAME, &encoded);
        }

        // Use a temporary buffer so we can compute the size.
        let mut tmp = Vec::new();
        headers.encode(&mut tmp);
        let size = VarInt::from_u32(tmp.len() as u32);

        Frame::HEADERS.encode(buf);
        size.encode(buf);
        buf.put_slice(&tmp);

        Ok(())
    }

    pub async fn write<S: AsyncWrite + Unpin>(&self, stream: &mut S) -> Result<(), ConnectError> {
        let mut buf = BytesMut::new();
        self.encode(&mut buf)?;
        stream.write_all_buf(&mut buf).await?;
        Ok(())
    }
}

impl From<Url> for ConnectRequest {
    fn from(url: Url) -> Self {
        Self {
            url,
            protocols: Vec::new(),
        }
    }
}

/// A CONNECT response to accept or reject a WebTransport session.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ConnectResponse {
    /// The status code of the response.
    pub status: http::status::StatusCode,

    /// The subprotocol selected by the server, if any
    pub protocol: Option<String>,
}

impl ConnectResponse {
    pub fn new(status: http::StatusCode) -> Self {
        Self {
            status,
            protocol: None,
        }
    }

    pub fn ok() -> Self {
        Self::new(http::StatusCode::OK)
    }

    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = Some(protocol.into());
        self
    }

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

        let protocol = headers
            .get(protocol_negotiation::SELECTED_NAME)
            .map(protocol_negotiation::decode_item)
            .transpose()
            .map_err(|_| ConnectError::InvalidProtocol)?;

        Ok(Self { status, protocol })
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

    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), ConnectError> {
        let mut headers = qpack::Headers::default();
        headers.set(":status", self.status.as_str());
        headers.set("sec-webtransport-http3-draft", "draft02");

        if let Some(protocol) = self.protocol.as_ref() {
            let encoded = protocol_negotiation::encode_item(protocol)?;
            headers.set(protocol_negotiation::SELECTED_NAME, &encoded);
        }

        // Use a temporary buffer so we can compute the size.
        let mut tmp = Vec::new();
        headers.encode(&mut tmp);
        let size = VarInt::from_u32(tmp.len() as u32);

        Frame::HEADERS.encode(buf);
        size.encode(buf);
        buf.put_slice(&tmp);

        Ok(())
    }

    pub async fn write<S: AsyncWrite + Unpin>(&self, stream: &mut S) -> Result<(), ConnectError> {
        let mut buf = BytesMut::new();
        self.encode(&mut buf)?;
        stream.write_all_buf(&mut buf).await?;
        Ok(())
    }
}

impl From<http::StatusCode> for ConnectResponse {
    fn from(status: http::StatusCode) -> Self {
        Self {
            status,
            protocol: None,
        }
    }
}

mod protocol_negotiation {
    //! WebTransport sub-protocol negotiation using RFC 8941 Structured Fields,
    //!
    //! according to [draft 14](https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-14.html#section-3.3)

    use sfv::{Item, ItemSerializer, List, ListEntry, ListSerializer, Parser, StringRef};

    use crate::ConnectError;

    /// The header name for the available protocols, sent within the WebTransport Connect request.
    pub const AVAILABLE_NAME: &str = "wt-available-protocols";
    /// The header name for the selected protocol, sent within the WebTransport Connect response.
    pub const SELECTED_NAME: &str = "wt-protocol";

    /// Encode a list of protocol strings as an RFC 8941 Structured Field List.
    pub fn encode_list(protocols: &[String]) -> Result<String, ConnectError> {
        let mut serializer = ListSerializer::new();
        for protocol in protocols {
            let s = StringRef::from_str(protocol)?;
            let _ = serializer.bare_item(s);
        }
        serializer.finish().ok_or(ConnectError::InvalidProtocol)
    }

    /// Decode an RFC 8941 Structured Field List of strings.
    pub fn decode_list(value: &str) -> Result<Vec<String>, ConnectError> {
        let list = Parser::new(value).parse::<List>()?;

        list.iter()
            .map(|entry| match entry {
                ListEntry::Item(item) => Ok(item
                    .bare_item
                    .as_string()
                    .ok_or(ConnectError::InvalidProtocol)?
                    .as_str()
                    .to_string()),
                _ => Err(ConnectError::InvalidProtocol),
            })
            .collect()
    }

    /// Encode a single string as an RFC 8941 Structured Field Item.
    pub fn encode_item(protocol: &str) -> Result<String, ConnectError> {
        let s = StringRef::from_str(protocol)?;
        Ok(ItemSerializer::new().bare_item(s).finish())
    }

    /// Decode an RFC 8941 Structured Field Item (single string).
    pub fn decode_item(value: &str) -> Result<String, ConnectError> {
        let item = Parser::new(value).parse::<Item>()?;
        Ok(item
            .bare_item
            .as_string()
            .ok_or(ConnectError::InvalidProtocol)?
            .as_str()
            .to_string())
    }
}
