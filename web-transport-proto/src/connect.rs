use std::sync::Arc;

use bytes::{Buf, BufMut, BytesMut};
use http::{
    uri::{Parts, Scheme},
    Extensions, HeaderMap, HeaderValue, Method, Uri,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use qpack::http_headers::{Header, HeaderError};
use url::Url;

use super::{Frame, VarInt};

use thiserror::Error;

// Errors that can occur during the connect request.
#[derive(Error, Debug)]
pub enum ConnectError {
    #[error("unexpected end of input")]
    UnexpectedEnd,

    #[error("qpack encoding error")]
    QpackEncodeError(#[from] qpack::EncoderError),

    #[error("qpack decoding error")]
    QpackDecodeError(#[from] qpack::DecoderError),

    #[error("Header parsing error")]
    HeaderParsingError(#[from] HeaderError),

    #[error("unexpected frame {0:?}")]
    UnexpectedFrame(Frame),

    #[error("invalid method")]
    InvalidMethod,

    #[error("Http error")]
    HttpError(#[from] http::Error),

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
    pub url: Url,
    pub headers: HeaderMap,
}

fn uri_to_url(uri: Uri) -> Result<Url, ConnectError> {
    let Parts {
        scheme,
        authority,
        path_and_query,
        ..
    } = uri.into_parts();
    let scheme = scheme.unwrap_or(Scheme::HTTPS);
    let Some(authority) = authority else {
        return Err(ConnectError::WrongAuthority);
    };
    let Some(path_and_query) = path_and_query else {
        return Err(ConnectError::WrongPath);
    };
    Ok(
        Url::parse(&format!("{scheme}://{authority}{path_and_query}"))
            .expect("Failed to parse URL"),
    )
}

fn url_to_uri(url: &Url) -> Result<Uri, ConnectError> {
    let mut path_and_query = url.path().to_owned();
    if let Some(query) = url.query() {
        path_and_query.push('?');
        path_and_query.push_str(query);
    }
    Ok(Uri::builder()
        .authority(url.authority())
        .scheme(url.scheme())
        .path_and_query(path_and_query)
        .build()?)
}

impl ConnectRequest {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, ConnectError> {
        let (typ, mut data) = Frame::read(buf).map_err(|_| ConnectError::UnexpectedEnd)?;
        if typ != Frame::HEADERS {
            return Err(ConnectError::UnexpectedFrame(typ));
        }

        // We no longer return UnexpectedEnd because we know the buffer should be large enough.

        let decoded = qpack::decode_stateless(&mut data, u64::MAX)?;
        let (method, uri, protocol, headers) =
            qpack::http_headers::Header::try_from(decoded.fields)?.into_request_parts()?;

        if method != http::Method::CONNECT {
            return Err(ConnectError::WrongMethod(Some(method)));
        }

        if protocol != Some("webtransport".to_owned()) {
            return Err(ConnectError::WrongProtocol(protocol.map(|s| s.to_string())));
        }

        Ok(Self {
            url: uri_to_url(uri)?,
            headers,
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
                Ok(request) => return Ok(request),
                Err(ConnectError::UnexpectedEnd) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), ConnectError> {
        // protocol header
        let mut ext = Extensions::new();
        ext.insert("webtransport".to_owned());
        let headers = Header::request(
            Method::CONNECT,
            url_to_uri(&self.url)?,
            self.headers.clone(),
            ext,
        )?;

        // Use a temporary buffer so we can compute the size.
        let mut tmp = Vec::new();
        headers.encode(&mut tmp)?;
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

#[derive(Debug)]
pub struct ConnectResponse {
    pub status: http::status::StatusCode,
    pub headers: HeaderMap,
}

impl ConnectResponse {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, ConnectError> {
        let (typ, mut data) = Frame::read(buf).map_err(|_| ConnectError::UnexpectedEnd)?;
        if typ != Frame::HEADERS {
            return Err(ConnectError::UnexpectedFrame(typ));
        }

        let decoded = qpack::decode_stateless(&mut data, u64::MAX)?;
        let (status, headers) =
            qpack::http_headers::Header::try_from(decoded.fields)?.into_response_parts()?;

        if !status.is_success() {
            return Err(ConnectError::WrongStatus(Some(status)));
        }

        Ok(Self { status, headers })
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
        let mut headers = self.headers.clone();
        headers.insert(
            "sec-webtransport-http3-draft",
            HeaderValue::from_static("draft02"),
        );

        let headers = Header::response(self.status, headers);

        // Use a temporary buffer so we can compute the size.
        let mut tmp = Vec::new();
        headers.encode(&mut tmp).expect("Header encoding failed");
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
