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

    #[error("header parsing error: field={field}, error={error}")]
    HeaderError { field: &'static str, error: String },

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
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct ConnectRequest {
    /// The URL to connect to.
    pub url: Url,
    /// The webtransport sub protocols requested (if any).
    pub protocols: Vec<String>,
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

        let protocols = if let Some(protocols) = headers
            .get(protocol_negotiation::AVAILABLE_NAME)
            .and_then(|protocols| {
                sfv::Parser::new(protocols)
                    .parse::<sfv::List>()
                    .inspect_err(|error| {
                        tracing::error!(
                            ?error,
                            "Failed to parse protocols as structured header field"
                        );
                    })
                    // if parsing of the field fails, the spec says we should ignore it and continue
                    .ok()
            }) {
            let total_items = protocols.len();
            let final_protocols: Vec<String> = protocols
                .into_iter()
                .filter_map(|item| match item {
                    sfv::ListEntry::Item(sfv::Item {
                        bare_item: sfv::BareItem::String(s),
                        ..
                    }) => Some(s.to_string()),
                    _ => None,
                })
                .collect();
            if final_protocols.len() != total_items {
                // we had non-string items in the list, according to the spec
                // we should ignore the entire list
                Vec::with_capacity(0)
            } else {
                final_protocols
            }
        } else {
            Vec::with_capacity(0)
        };

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
            // generate a proper StructuredField List header of the protocols given
            let mut items = Vec::new();
            for protocol in &self.protocols {
                items.push(sfv::ListEntry::Item(sfv::Item::new(
                    sfv::StringRef::from_str(protocol.as_str()).map_err(|err| {
                        ConnectError::HeaderError {
                            field: protocol_negotiation::AVAILABLE_NAME,
                            error: err.to_string(),
                        }
                    })?,
                )));
            }
            let mut ser = sfv::ListSerializer::new();
            ser.members(items.iter());
            if let Some(protocols) = ser.finish() {
                headers.set(protocol_negotiation::AVAILABLE_NAME, protocols.as_str());
            }
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

#[derive(Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct ConnectResponse {
    /// The status code of the response.
    pub status: http::status::StatusCode,
    /// The webtransport sub protocol selected by the server, if any
    pub protocol: Option<String>,
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

        let protocol = headers
            .get(protocol_negotiation::SELECTED_NAME)
            .and_then(|s| {
                let item = sfv::Parser::new(s)
                    .parse::<sfv::Item>()
                    .map_err(|error| {
                        tracing::error!(?error, "Failed to parse protocol header item. ignoring");
                    })
                    .ok()?;
                item.bare_item.as_string().map(|rf| rf.to_string())
            });

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
        if let Some(protocol) = &self.protocol {
            let serialized_item = sfv::ItemSerializer::new()
                .bare_item(
                    sfv::StringRef::from_str(protocol.as_str()).map_err(|error| {
                        ConnectError::HeaderError {
                            field: protocol_negotiation::SELECTED_NAME,
                            error: error.to_string(),
                        }
                    })?,
                )
                .finish();
            headers.set(
                protocol_negotiation::SELECTED_NAME,
                serialized_item.as_str(),
            );
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

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;

    #[test]
    pub fn test_request_encode_decode_simple() {
        let response = ConnectRequest {
            url: "https://example.com".parse().unwrap(),
            protocols: vec![],
        };
        let mut buf = BytesMut::new();
        response.encode(&mut buf).unwrap();
        let decoded = ConnectRequest::decode(&mut buf).unwrap();
        assert_eq!(response, decoded);
    }

    #[test]
    pub fn test_request_encode_decode_with_protocol() {
        let response = ConnectRequest {
            url: "https://example.com".parse().unwrap(),
            protocols: vec!["protocol-1".to_string(), "protocol-2".to_string()],
        };
        let mut buf = BytesMut::new();
        response.encode(&mut buf).unwrap();
        let decoded = ConnectRequest::decode(&mut buf).unwrap();
        assert_eq!(response, decoded);
    }

    #[test]
    pub fn test_request_encode_decode_with_protocol_with_quotes() {
        let response = ConnectRequest {
            url: "https://example.com".parse().unwrap(),
            protocols: vec!["protocol-\"1\"".to_string(), "protocol-'2'".to_string()],
        };
        let mut buf = BytesMut::new();
        response.encode(&mut buf).unwrap();
        let decoded = ConnectRequest::decode(&mut buf).unwrap();
        assert_eq!(response, decoded);
    }

    #[test]
    pub fn test_request_encode_decode_with_non_compliant_protocol() {
        let response = ConnectRequest {
            url: "https://example.com".parse().unwrap(),
            protocols: vec!["protocol-🐕".to_string()],
        };
        let mut buf = BytesMut::new();
        let resp = response.encode(&mut buf);
        assert!(resp.is_err(), "non ascii must fail");
        assert!(matches!(
            resp,
            Err(ConnectError::HeaderError {
                field: protocol_negotiation::AVAILABLE_NAME,
                ..
            })
        ));
    }
    #[test]
    pub fn test_response_encode_decode_simple() {
        let response = ConnectResponse {
            status: StatusCode::ACCEPTED,
            protocol: None,
        };
        let mut buf = BytesMut::new();
        response.encode(&mut buf).unwrap();
        let decoded = ConnectResponse::decode(&mut buf).unwrap();
        assert_eq!(response, decoded);
    }

    #[test]
    pub fn test_response_encode_decode_with_protocol() {
        let response = ConnectResponse {
            status: StatusCode::ACCEPTED,
            protocol: Some("proto".to_string()),
        };
        let mut buf = BytesMut::new();
        response.encode(&mut buf).unwrap();
        let decoded = ConnectResponse::decode(&mut buf).unwrap();
        assert_eq!(response, decoded);
    }

    #[test]
    pub fn test_response_encode_decode_with_protocol_with_quotes() {
        let response = ConnectResponse {
            status: StatusCode::ACCEPTED,
            protocol: Some("'proto'-\"1\"".to_string()),
        };
        let mut buf = BytesMut::new();
        response.encode(&mut buf).unwrap();
        let decoded = ConnectResponse::decode(&mut buf).unwrap();
        assert_eq!(response, decoded);
    }

    #[test]
    pub fn test_response_encode_decode_with_noncompliant_protocol() {
        let response = ConnectResponse {
            status: StatusCode::ACCEPTED,
            protocol: Some("proto-😅".to_string()),
        };
        let mut buf = BytesMut::new();
        let resp = response.encode(&mut buf);
        assert!(resp.is_err(), "non ascii must fail");
        assert!(matches!(
            resp,
            Err(ConnectError::HeaderError {
                field: protocol_negotiation::SELECTED_NAME,
                ..
            })
        ));
    }
}
