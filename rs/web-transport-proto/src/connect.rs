use std::{fmt, str::FromStr, sync::Arc};

use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::{Position, Url};

use super::{qpack, Frame, VarInt, MAX_FRAME_SIZE};

use thiserror::Error;

// RFC 9114 section 4.2.2: each decoded field costs name + value + 32 bytes.
pub(crate) const MAX_FIELD_SECTION_SIZE: u64 = 16 * 1024;

/// An error parsing or encoding a CONNECT request or response.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
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

    /// The response has no status pseudo-header.
    #[error("missing status")]
    MissingStatus,

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

    #[error("frame too large")]
    FrameTooLarge,

    /// A generic header conflicts with a field managed by the CONNECT encoder.
    #[error("reserved CONNECT header: {0}")]
    ReservedHeader(http::HeaderName),

    /// A pseudo-header is duplicated, unknown, or follows an ordinary field.
    #[error("invalid pseudo-header")]
    InvalidPseudoHeader,

    #[error("io error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("invalid http header value")]
    InvalidHttpHeaderValue,

    #[error("invalid http header name")]
    InvalidHttpHeaderName,

    /// The encoded HEADERS frame or its decoded field section exceeds the CONNECT limit.
    #[error("field section too large")]
    FieldSectionTooLarge,
}

impl From<qpack::FieldSectionError> for ConnectError {
    fn from(err: qpack::FieldSectionError) -> Self {
        match err {
            qpack::FieldSectionError::Decode(err) => Self::QpackError(err),
            qpack::FieldSectionError::TooLarge => Self::FieldSectionTooLarge,
        }
    }
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
///
/// The [`Debug`](fmt::Debug) output is safe to log. It keeps the URL's scheme,
/// host, port, and path, the protocols, and the header names, but redacts the
/// URL's userinfo, query, and fragment. Header values are redacted too, except
/// `origin` and `user-agent` when not marked
/// [sensitive](http::HeaderValue::set_sensitive).
#[non_exhaustive]
#[derive(Clone)]
pub struct ConnectRequest {
    /// The URL to connect to.
    pub url: Url,

    /// The subprotocols requested (if any).
    pub protocols: Vec<String>,

    /// Ordinary HTTP/3 request headers, including repeated values.
    ///
    /// Set `protocols` instead of inserting the reserved `wt-available-protocols`
    /// field; encoding a generic entry for that name returns [`ConnectError::ReservedHeader`].
    pub headers: http::HeaderMap,
}

impl ConnectRequest {
    pub fn new(url: impl Into<Url>) -> Self {
        Self {
            url: url.into(),
            protocols: Vec::new(),
            headers: http::HeaderMap::new(),
        }
    }

    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocols.push(protocol.into());
        self
    }

    pub fn with_protocols(mut self, protocols: impl IntoIterator<Item = String>) -> Self {
        self.protocols.extend(protocols);
        self
    }

    pub fn with_header(mut self, name: http::HeaderName, value: http::HeaderValue) -> Self {
        self.headers.append(name, value);
        self
    }

    pub fn with_headers(mut self, headers: http::HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, ConnectError> {
        let mut data = decode_headers_frame(buf)?;
        Self::decode_headers(&mut data)
    }

    fn decode_headers<B: Buf>(data: &mut B) -> Result<Self, ConnectError> {
        let headers = qpack::Headers::decode(data)?;

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

        // Save all headers, excluding pseudo-headers and protocol negotiation headers
        // (protocol negotiation is handled via the `protocols` field).
        let mut raw_headers = http::HeaderMap::new();
        for (item_header_name, item_header_value) in headers.fields.iter() {
            if item_header_name.starts_with(':') {
                continue;
            }
            if item_header_name == protocol_negotiation::AVAILABLE_NAME {
                continue;
            }
            let header_name = http::HeaderName::from_bytes(item_header_name.as_bytes())
                .map_err(|_| ConnectError::InvalidHttpHeaderName)?;
            let header_value = http::HeaderValue::from_str(item_header_value)
                .map_err(|_| ConnectError::InvalidHttpHeaderValue)?;
            raw_headers.append(header_name, header_value);
        }

        Ok(Self {
            url,
            protocols,
            headers: raw_headers,
        })
    }

    /// Read a CONNECT request from a stream, consuming only the exact bytes of the frame.
    pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Self, ConnectError> {
        let buf = read_headers_frame(stream).await?;
        Self::decode_headers(&mut buf.as_slice())
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), ConnectError> {
        let mut headers = qpack::Headers::default();
        for (item_header_name, item_header_value) in self.headers.iter() {
            if item_header_name == protocol_negotiation::AVAILABLE_NAME {
                return Err(ConnectError::ReservedHeader(item_header_name.clone()));
            }
            // QPACK's string encoder requires visible ASCII (with internal tabs).
            let item_header_value_str = item_header_value
                .to_str()
                .map_err(|_| ConnectError::InvalidHttpHeaderValue)?;
            headers.append(item_header_name.as_str(), item_header_value_str);
        }
        headers.append(":method", "CONNECT");
        headers.append(":scheme", self.url.scheme());
        headers.append(":authority", self.url.authority());
        let path_and_query = match self.url.query() {
            Some(query) => format!("{}?{}", self.url.path(), query),
            None => self.url.path().to_string(),
        };
        headers.append(":path", &path_and_query);
        headers.append(":protocol", "webtransport");

        if !self.protocols.is_empty() {
            let encoded = protocol_negotiation::encode_list(&self.protocols)?;
            headers.append(protocol_negotiation::AVAILABLE_NAME, &encoded);
        }

        encode_headers_frame(&headers, buf)
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
            headers: http::HeaderMap::new(),
        }
    }
}

/// Header values shown by [`ConnectRequest`]'s `Debug` output.
///
/// This is an allowlist so that credentials in unknown or custom headers
/// (`authorization`, `cookie`, `x-api-key`, ...) are redacted by default.
const DEBUG_VISIBLE_HEADERS: [http::HeaderName; 2] =
    [http::header::ORIGIN, http::header::USER_AGENT];

/// Replaces a redacted value in `Debug` output.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Formats a URL with its userinfo, query, and fragment redacted.
struct RedactedUrl<'a>(&'a Url);

impl fmt::Debug for RedactedUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.0;
        let mut redacted = String::from(&url[..Position::BeforeUsername]);
        if !url.username().is_empty() || url.password().is_some() {
            redacted.push_str("[redacted]@");
        }
        redacted.push_str(&url[Position::BeforeHost..Position::AfterPath]);
        if url.query().is_some() {
            redacted.push_str("?[redacted]");
        }
        if url.fragment().is_some() {
            redacted.push_str("#[redacted]");
        }
        fmt::Debug::fmt(&redacted, f)
    }
}

/// Formats headers with every value outside [`DEBUG_VISIBLE_HEADERS`] redacted.
struct RedactedHeaders<'a>(&'a http::HeaderMap);

impl fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (name, value) in self.0 {
            if DEBUG_VISIBLE_HEADERS.contains(name) && !value.is_sensitive() {
                map.entry(name, value);
            } else {
                map.entry(name, &Redacted);
            }
        }
        map.finish()
    }
}

impl fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectRequest")
            .field("url", &RedactedUrl(&self.url))
            .field("protocols", &self.protocols)
            .field("headers", &RedactedHeaders(&self.headers))
            .finish()
    }
}

/// A CONNECT response to accept or reject a WebTransport session.
///
/// Header values are redacted in [`Debug`](fmt::Debug), as for [`ConnectRequest`].
#[non_exhaustive]
#[derive(Clone)]
pub struct ConnectResponse {
    /// The status code of the response.
    pub status: http::StatusCode,

    /// The subprotocol selected by the server, if any.
    pub protocol: Option<String>,

    /// Ordinary HTTP/3 response headers, including repeated values.
    ///
    /// `wt-protocol` and `sec-webtransport-http3-draft` are managed by the encoder
    /// and must not be inserted here; pseudo-headers cannot be represented by a
    /// [`http::HeaderName`]. Values unsupported by the QPACK encoder return
    /// [`ConnectError::InvalidHttpHeaderValue`].
    pub headers: http::HeaderMap,
}

impl ConnectResponse {
    /// Create a response with the given status and no application headers.
    pub fn new(status: http::StatusCode) -> Self {
        Self {
            status,
            protocol: None,
            headers: http::HeaderMap::new(),
        }
    }

    /// Select the subprotocol negotiated with the client.
    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = Some(protocol.into());
        self
    }

    /// Append an ordinary response header without replacing earlier values.
    pub fn with_header(mut self, name: http::HeaderName, value: http::HeaderValue) -> Self {
        self.headers.append(name, value);
        self
    }

    /// Extend the response headers, replacing values for names present in the map.
    pub fn with_headers(mut self, headers: http::HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Decode one HEADERS frame, preserving both successful and rejection responses.
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, ConnectError> {
        let mut data = decode_headers_frame(buf)?;
        let response = Self::decode_headers(&mut data);
        data.advance(data.remaining());
        response
    }

    fn decode_headers<B: Buf>(data: &mut B) -> Result<Self, ConnectError> {
        let fields = qpack::Headers::decode(data)?;
        let mut status = None;
        let mut protocol = None;
        let mut headers = http::HeaderMap::new();
        let mut ordinary = false;
        let mut draft = false;

        for (name, value) in fields.fields {
            if name.starts_with(':') {
                if ordinary || name != ":status" || status.is_some() {
                    return Err(ConnectError::InvalidPseudoHeader);
                }
                status = Some(
                    http::StatusCode::from_str(&value).map_err(|_| ConnectError::InvalidStatus)?,
                );
                continue;
            }
            ordinary = true;
            // HeaderName would normalize uppercase names instead of rejecting them.
            if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
                return Err(ConnectError::InvalidHttpHeaderName);
            }
            let name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ConnectError::InvalidHttpHeaderName)?;
            let value = http::HeaderValue::from_str(&value)
                .map_err(|_| ConnectError::InvalidHttpHeaderValue)?;
            validate_response_header(&name, &value)?;
            if name == protocol_negotiation::SELECTED_NAME {
                if protocol.is_some() {
                    return Err(ConnectError::InvalidProtocol);
                }
                protocol = Some(
                    protocol_negotiation::decode_item(
                        value.to_str().map_err(|_| ConnectError::InvalidProtocol)?,
                    )
                    .map_err(|_| ConnectError::InvalidProtocol)?,
                );
            } else if name == LEGACY_DRAFT_HEADER {
                if draft {
                    return Err(ConnectError::ReservedHeader(name));
                }
                draft = true;
            } else {
                headers.append(name, value);
            }
        }

        Ok(Self {
            status: status.ok_or(ConnectError::MissingStatus)?,
            protocol,
            headers,
        })
    }

    /// Read one response, consuming only its frame bytes, including on rejection.
    pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Self, ConnectError> {
        let buf = read_headers_frame(stream).await?;
        Self::decode_headers(&mut buf.as_slice())
    }

    /// Encode the response, rejecting reserved generic fields before writing any bytes.
    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), ConnectError> {
        let mut headers = qpack::Headers::default();
        headers.append(":status", self.status.as_str());
        headers.append(LEGACY_DRAFT_HEADER, "draft02");

        if let Some(protocol) = self.protocol.as_ref() {
            let encoded = protocol_negotiation::encode_item(protocol)?;
            headers.append(protocol_negotiation::SELECTED_NAME, &encoded);
        }
        for (name, value) in &self.headers {
            if name == protocol_negotiation::SELECTED_NAME || name == LEGACY_DRAFT_HEADER {
                return Err(ConnectError::ReservedHeader(name.clone()));
            }
            validate_response_header(name, value)?;
            let value = value
                .to_str()
                .map_err(|_| ConnectError::InvalidHttpHeaderValue)?;
            headers.append(name.as_str(), value);
        }

        encode_headers_frame(&headers, buf)
    }

    /// Write the response after validating its complete field section.
    pub async fn write<S: AsyncWrite + Unpin>(&self, stream: &mut S) -> Result<(), ConnectError> {
        let mut buf = BytesMut::new();
        self.encode(&mut buf)?;
        stream.write_all_buf(&mut buf).await?;
        Ok(())
    }
}

const LEGACY_DRAFT_HEADER: &str = "sec-webtransport-http3-draft";

fn validate_response_header(
    name: &http::HeaderName,
    value: &http::HeaderValue,
) -> Result<(), ConnectError> {
    // HTTP/3 forbids connection-specific fields; TE is only defined in requests.
    if matches!(
        name.as_str(),
        "connection" | "proxy-connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "te"
    ) {
        return Err(ConnectError::InvalidHttpHeaderName);
    }
    if value
        .as_bytes()
        .first()
        .is_some_and(|b| matches!(b, b' ' | b'\t'))
        || value
            .as_bytes()
            .last()
            .is_some_and(|b| matches!(b, b' ' | b'\t'))
    {
        return Err(ConnectError::InvalidHttpHeaderValue);
    }
    Ok(())
}

impl fmt::Debug for ConnectResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectResponse")
            .field("status", &self.status)
            .field("protocol", &self.protocol)
            .field("headers", &RedactedHeaders(&self.headers))
            .finish()
    }
}

impl Default for ConnectResponse {
    fn default() -> Self {
        Self::new(http::StatusCode::OK)
    }
}

impl From<http::StatusCode> for ConnectResponse {
    fn from(status: http::StatusCode) -> Self {
        Self::new(status)
    }
}

fn check_frame_size(typ: Frame, size: u64) -> Result<(), ConnectError> {
    if typ == Frame::HEADERS && size > MAX_FIELD_SECTION_SIZE {
        return Err(ConnectError::FieldSectionTooLarge);
    }
    if size > MAX_FRAME_SIZE {
        return Err(ConnectError::FrameTooLarge);
    }
    Ok(())
}

fn encode_headers_frame<B: BufMut>(
    headers: &qpack::Headers,
    buf: &mut B,
) -> Result<(), ConnectError> {
    // Use a temporary buffer so we can compute the size and write nothing on error.
    let mut tmp = Vec::new();
    headers.encode(&mut tmp)?;
    let size = VarInt::from_u32(tmp.len() as u32);

    Frame::HEADERS.encode(buf);
    size.encode(buf);
    buf.put_slice(&tmp);

    Ok(())
}

fn decode_headers_frame<B: Buf>(buf: &mut B) -> Result<bytes::buf::Take<&mut B>, ConnectError> {
    let (typ, data) = Frame::read_bounded(buf, MAX_FIELD_SECTION_SIZE, MAX_FRAME_SIZE).map_err(
        |err| match err {
            crate::frame::FrameReadError::UnexpectedEnd => ConnectError::UnexpectedEnd,
            crate::frame::FrameReadError::TooLarge(Frame::HEADERS) => {
                ConnectError::FieldSectionTooLarge
            }
            crate::frame::FrameReadError::TooLarge(_) => ConnectError::FrameTooLarge,
        },
    )?;
    if typ != Frame::HEADERS {
        return Err(ConnectError::UnexpectedFrame(typ));
    }
    Ok(data)
}

/// Read the next HEADERS frame from the stream, skipping any GREASE frames.
///
/// Returns the raw payload bytes of the HEADERS frame.
async fn read_headers_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, ConnectError> {
    loop {
        let typ = Frame(
            VarInt::read(stream)
                .await
                .map_err(|_| ConnectError::UnexpectedEnd)?,
        );
        let size = VarInt::read(stream)
            .await
            .map_err(|_| ConnectError::UnexpectedEnd)?;

        let size = size.into_inner();
        check_frame_size(typ, size)?;

        let mut payload = stream.take(size);

        if typ.is_grease() {
            let n = tokio::io::copy(&mut payload, &mut tokio::io::sink()).await?;
            if n < size {
                return Err(ConnectError::UnexpectedEnd);
            }
            continue;
        }

        let mut buf = Vec::with_capacity(size as usize);
        payload.read_to_end(&mut buf).await?;

        if buf.len() < size as usize {
            return Err(ConnectError::UnexpectedEnd);
        }

        if typ != Frame::HEADERS {
            return Err(ConnectError::UnexpectedFrame(typ));
        }

        return Ok(buf);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a framed CONNECT request on the wire.
    fn encode_request(url: &str) -> Vec<u8> {
        let req = ConnectRequest::new(Url::parse(url).unwrap());
        let mut buf = Vec::new();
        req.encode(&mut buf).unwrap();
        buf
    }

    /// Build a framed CONNECT response on the wire.
    fn encode_response() -> Vec<u8> {
        let resp = ConnectResponse::default();
        let mut buf = Vec::new();
        resp.encode(&mut buf).unwrap();
        buf
    }

    /// Encode a GREASE frame: type (0x21 = first GREASE value) + length + payload.
    fn encode_grease_frame(payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        VarInt::from_u32(0x21).encode(&mut buf); // GREASE frame type
        VarInt::from_u32(payload.len() as u32).encode(&mut buf);
        buf.extend_from_slice(payload);
        buf
    }

    fn reframe(payload: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        VarInt::from_u32(payload.len() as u32).encode(&mut wire);
        wire.extend_from_slice(payload);
        wire
    }

    fn payload(mut wire: &[u8]) -> Vec<u8> {
        let (_, mut data) = Frame::read(&mut wire).unwrap();
        data.copy_to_bytes(data.remaining()).to_vec()
    }

    fn append_literal(payload: &mut Vec<u8>, name: &[u8], value: &[u8]) {
        qpack::encode_prefix(payload, 3, 0b00100, name.len());
        payload.extend_from_slice(name);
        qpack::encode_prefix(payload, 7, 0, value.len());
        payload.extend_from_slice(value);
    }

    fn assert_section_too_large<T: std::fmt::Debug>(result: Result<T, ConnectError>) {
        assert_eq!(
            result
                .expect_err("oversized section was accepted")
                .to_string(),
            "field section too large"
        );
    }

    #[test]
    fn rejection_is_a_decoded_response() {
        let mut wire = Vec::new();
        ConnectResponse::new(http::StatusCode::FORBIDDEN)
            .encode(&mut wire)
            .unwrap();
        assert_eq!(
            ConnectResponse::decode(&mut wire.as_slice())
                .unwrap()
                .status,
            http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn qpack_preserves_repeated_field_lines() {
        let mut section = vec![0, 0];
        append_literal(&mut section, b"set-cookie", b"a=1");
        append_literal(&mut section, b"set-cookie", b"b=2");
        let headers = qpack::Headers::decode(&mut section.as_slice()).unwrap();
        assert_eq!(
            headers
                .fields
                .iter()
                .filter(|(name, _)| name.as_str() == "set-cookie")
                .count(),
            2
        );
    }

    fn assert_response_headers(response: &ConnectResponse) {
        assert_eq!(response.protocol.as_deref(), Some("moq"));
        assert!(!response.headers.contains_key("wt-protocol"));
        assert!(!response.headers.contains_key(LEGACY_DRAFT_HEADER));
        assert_eq!(
            response
                .headers
                .get_all("set-cookie")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["a=1", "b=2"]
        );
        assert_eq!(response.headers["x-diagnostic"], "retry elsewhere");
    }

    #[tokio::test]
    async fn response_headers_roundtrip_and_exact_consumption() {
        for status in [
            http::StatusCode::OK,
            http::StatusCode::FORBIDDEN,
            http::StatusCode::TEMPORARY_REDIRECT,
        ] {
            let mut headers = http::HeaderMap::new();
            headers.append(
                http::header::SET_COOKIE,
                http::HeaderValue::from_static("a=1"),
            );
            headers.append(
                http::header::SET_COOKIE,
                http::HeaderValue::from_static("b=2"),
            );
            let response = ConnectResponse::new(status)
                .with_protocol("moq")
                .with_headers(headers)
                .with_header(
                    http::HeaderName::from_static("x-diagnostic"),
                    http::HeaderValue::from_static("retry elsewhere"),
                );
            let mut wire = Vec::new();
            response.write(&mut wire).await.unwrap();
            wire.extend_from_slice(b"trailing");
            let mut buf = wire.as_slice();
            let response = ConnectResponse::decode(&mut buf).unwrap();
            assert_eq!(response.status, status);
            assert_response_headers(&response);
            assert_eq!(buf, b"trailing");
            let mut cursor = Cursor::new(wire);
            let response = ConnectResponse::read(&mut cursor).await.unwrap();
            assert_eq!(response.status, status);
            assert_response_headers(&response);
            let mut tail = Vec::new();
            cursor.read_to_end(&mut tail).await.unwrap();
            assert_eq!(tail, b"trailing");
            let mut wire = Vec::new();
            response.encode(&mut wire).unwrap();
            assert_response_headers(&ConnectResponse::decode(&mut wire.as_slice()).unwrap());
        }
    }

    #[test]
    fn request_preserves_repeated_ordinary_fields() {
        let request = ConnectRequest::new(Url::parse("https://example.com/").unwrap())
            .with_header(
                http::header::ACCEPT,
                http::HeaderValue::from_static("text/plain"),
            )
            .with_header(
                http::header::ACCEPT,
                http::HeaderValue::from_static("text/html"),
            );
        let mut wire = Vec::new();
        request.encode(&mut wire).unwrap();
        let request = ConnectRequest::decode(&mut wire.as_slice()).unwrap();
        assert_eq!(
            request
                .headers
                .get_all("accept")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["text/plain", "text/html"]
        );
    }

    #[tokio::test]
    async fn reserved_generic_fields_write_nothing() {
        for name in ["wt-protocol", LEGACY_DRAFT_HEADER] {
            let response = ConnectResponse::default().with_header(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static("override"),
            );
            let mut wire = Vec::new();
            assert!(matches!(
                response.encode(&mut wire),
                Err(ConnectError::ReservedHeader(_))
            ));
            assert!(matches!(
                response.write(&mut wire).await,
                Err(ConnectError::ReservedHeader(_))
            ));
            assert!(wire.is_empty());
        }
        let request = ConnectRequest::new(Url::parse("https://example.com/").unwrap()).with_header(
            http::HeaderName::from_static("wt-available-protocols"),
            http::HeaderValue::from_static("override"),
        );
        let mut wire = Vec::new();
        assert!(matches!(
            request.encode(&mut wire),
            Err(ConnectError::ReservedHeader(_))
        ));
        assert!(matches!(
            request.write(&mut wire).await,
            Err(ConnectError::ReservedHeader(_))
        ));
        assert!(wire.is_empty());
    }

    #[tokio::test]
    async fn response_rejects_malformed_field_lines() {
        let cases: &[&[(&[u8], &[u8])]] = &[
            &[(b"x", b"no status")],
            &[(b":status", b"bad")],
            &[(b":status", b"200"), (b":status", b"403")],
            &[(b":path", b"/"), (b":status", b"200")],
            &[(b"x", b"first"), (b":status", b"200")],
            &[(b":status", b"403"), (b"wt-protocol", b"unquoted")],
            &[
                (b":status", b"200"),
                (b"wt-protocol", b"\"a\""),
                (b"wt-protocol", b"\"b\""),
            ],
            &[(b":status", b"200"), (b"wt-protocol", b"\"a\", \"b\"")],
            &[(b":status", b"403"), (b"X-Invalid", b"value")],
            &[(b":status", b"200"), (b"bad name", b"value")],
            &[(b":status", b"403"), (b"x", b"bad\r\nvalue")],
            &[(b":status", b"403"), (b"x", b"\x00")],
            &[(b":status", b"200"), (b"x", b" leading")],
            &[(b":status", b"200"), (b"x", b"trailing\t")],
            &[(b":status", b"200"), (b"x", b"\xff")],
            &[
                (b":status", b"200"),
                (b"sec-webtransport-http3-draft", b"draft02"),
                (b"sec-webtransport-http3-draft", b"draft02"),
            ],
        ];
        for fields in cases {
            let mut section = vec![0, 0];
            for (name, value) in *fields {
                append_literal(&mut section, name, value);
            }
            let mut wire = reframe(&section);
            wire.extend_from_slice(b"trailing");
            let mut buf = wire.as_slice();
            assert!(
                ConnectResponse::decode(&mut buf).is_err(),
                "accepted {fields:?}"
            );
            assert_eq!(buf, b"trailing");
            let mut cursor = Cursor::new(wire);
            assert!(
                ConnectResponse::read(&mut cursor).await.is_err(),
                "accepted {fields:?}"
            );
            let mut tail = Vec::new();
            cursor.read_to_end(&mut tail).await.unwrap();
            assert_eq!(tail, b"trailing");
        }
    }

    #[tokio::test]
    async fn malformed_qpack_consumes_only_its_frame() {
        // A dynamic reference is unsupported, with unread bytes left in the section.
        let mut wire = reframe(&[0, 0, 0x80, 0xc0 | 25]);
        wire.extend_from_slice(b"trailing");
        let mut buf = wire.as_slice();
        assert!(matches!(
            ConnectResponse::decode(&mut buf),
            Err(ConnectError::QpackError(_))
        ));
        assert_eq!(buf, b"trailing");
        let mut cursor = Cursor::new(wire);
        assert!(matches!(
            ConnectResponse::read(&mut cursor).await,
            Err(ConnectError::QpackError(_))
        ));
        let mut tail = Vec::new();
        cursor.read_to_end(&mut tail).await.unwrap();
        assert_eq!(tail, b"trailing");
    }

    #[test]
    fn response_rejects_connection_specific_fields() {
        for name in [
            "connection",
            "proxy-connection",
            "keep-alive",
            "transfer-encoding",
            "upgrade",
            "te",
        ] {
            let response = ConnectResponse::default().with_header(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static("value"),
            );
            let mut wire = Vec::new();
            assert!(matches!(
                response.encode(&mut wire),
                Err(ConnectError::InvalidHttpHeaderName)
            ));
            assert!(wire.is_empty());
            let mut section = vec![0, 0];
            append_literal(&mut section, b":status", b"403");
            append_literal(&mut section, name.as_bytes(), b"value");
            assert!(matches!(
                ConnectResponse::decode(&mut reframe(&section).as_slice()),
                Err(ConnectError::InvalidHttpHeaderName)
            ));
        }
    }

    #[tokio::test]
    async fn response_generic_values_validate_before_writing() {
        for value in [
            http::HeaderValue::from_bytes(b"\xff").unwrap(),
            http::HeaderValue::from_static(" leading"),
            http::HeaderValue::from_static("trailing "),
        ] {
            let response =
                ConnectResponse::default().with_header(http::HeaderName::from_static("x"), value);
            let mut wire = Vec::new();
            assert!(matches!(
                response.encode(&mut wire),
                Err(ConnectError::InvalidHttpHeaderValue)
            ));
            assert!(matches!(
                response.write(&mut wire).await,
                Err(ConnectError::InvalidHttpHeaderValue)
            ));
            assert!(wire.is_empty());
        }
    }

    #[tokio::test]
    async fn repeated_response_headers_obey_exact_size_limit() {
        // Charge every field line, including both occurrences of x-pad.
        let overhead = (32 + ":status".len() + 3)
            + (32 + LEGACY_DRAFT_HEADER.len() + "draft02".len())
            + 2 * (32 + "x-pad".len());
        let fill = MAX_FIELD_SECTION_SIZE as usize - overhead - 1;
        for status in [http::StatusCode::OK, http::StatusCode::FORBIDDEN] {
            let response = |len| {
                ConnectResponse::new(status)
                    .with_header(
                        http::HeaderName::from_static("x-pad"),
                        http::HeaderValue::from_static("a"),
                    )
                    .with_header(
                        http::HeaderName::from_static("x-pad"),
                        http::HeaderValue::from_str(&"b".repeat(len)).unwrap(),
                    )
            };
            let mut wire = Vec::new();
            response(fill).encode(&mut wire).unwrap();
            let decoded = ConnectResponse::decode(&mut wire.as_slice()).unwrap();
            assert_eq!(decoded.headers.get_all("x-pad").iter().count(), 2);
            let mut wire = Vec::new();
            assert_section_too_large(response(fill + 1).encode(&mut wire));
            assert_section_too_large(response(fill + 1).write(&mut wire).await);
            assert!(wire.is_empty());
        }
    }

    #[test]
    fn response_debug_redacts_rejection_secrets() {
        let response = ConnectResponse::new(http::StatusCode::FORBIDDEN).with_header(
            http::header::SET_COOKIE,
            http::HeaderValue::from_static("SECRET"),
        );
        let debug = format!("{response:?}");
        assert!(debug.contains("403"));
        assert!(debug.contains("set-cookie"));
        assert!(!debug.contains("SECRET"));
    }

    #[test]
    fn connect_rejects_literal_field_accordion() {
        let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
        for base in [encode_request("https://example.com/"), encode_response()] {
            let mut headers = payload(&base);
            for i in 0..4_000 {
                append_literal(&mut headers, &[alphabet[i / 64], alphabet[i % 64]], b"");
            }
            assert!(headers.len() < 16 * 1024);
            let wire = reframe(&headers);
            assert_section_too_large(ConnectRequest::decode(&mut wire.as_slice()));
            assert_section_too_large(ConnectResponse::decode(&mut wire.as_slice()));
        }
    }

    #[test]
    fn response_charges_repeated_static_fields() {
        let mut headers = payload(&encode_response());
        // The indexed `:status: 200` line is one byte but costs 42 decoded bytes.
        let status = headers[2];
        assert_eq!(status, 0b1100_0000 | 25);
        headers.resize(headers.len() + 500, status);
        assert!(headers.len() < MAX_FIELD_SECTION_SIZE as usize);
        assert_section_too_large(ConnectResponse::decode(&mut reframe(&headers).as_slice()));
    }

    #[tokio::test]
    async fn request_encode_refuses_field_section_over_limit() {
        // The pseudo-headers for https://example.com/ cost 234 bytes; `x-pad` costs 37 plus its value.
        let fill = MAX_FIELD_SECTION_SIZE as usize - 234 - 37;
        let request = |len: usize| {
            ConnectRequest::new(Url::parse("https://example.com/").unwrap()).with_header(
                http::HeaderName::from_static("x-pad"),
                http::HeaderValue::from_str(&"a".repeat(len)).unwrap(),
            )
        };

        let mut wire = Vec::new();
        request(fill).encode(&mut wire).unwrap();
        let decoded = ConnectRequest::decode(&mut wire.as_slice()).unwrap();
        assert_eq!(decoded.headers["x-pad"].len(), fill);

        let mut wire = Vec::new();
        assert_section_too_large(request(fill + 1).encode(&mut wire));
        assert_section_too_large(request(fill + 1).write(&mut wire).await);
        assert!(
            wire.is_empty(),
            "a refused request wrote {} bytes",
            wire.len()
        );
    }

    #[tokio::test]
    async fn response_encode_refuses_field_section_over_limit() {
        let response =
            ConnectResponse::default().with_protocol("a".repeat(MAX_FIELD_SECTION_SIZE as usize));
        let mut wire = Vec::new();
        assert_section_too_large(response.encode(&mut wire));
        assert_section_too_large(response.write(&mut wire).await);
        assert!(
            wire.is_empty(),
            "a refused response wrote {} bytes",
            wire.len()
        );
    }

    // RFC 7541 Appendix B gives 'a' the five-bit code 00011.
    fn huffman_a(count: usize) -> Vec<u8> {
        let bits = count * 5;
        let mut encoded = vec![0; bits.div_ceil(8)];
        for i in 0..count {
            for bit in [3, 4] {
                let position = i * 5 + bit;
                encoded[position / 8] |= 1 << (7 - position % 8);
            }
        }
        for position in bits..encoded.len() * 8 {
            encoded[position / 8] |= 1 << (7 - position % 8);
        }
        encoded
    }

    #[test]
    fn connect_rejects_huffman_expansion() {
        for base in [encode_request("https://example.com/"), encode_response()] {
            let mut headers = payload(&base);
            qpack::encode_prefix(&mut headers, 3, 0b00100, 1);
            headers.push(b'x');
            let encoded = huffman_a(17_000);
            qpack::encode_prefix(&mut headers, 7, 1, encoded.len());
            headers.extend_from_slice(&encoded);
            assert!(headers.len() < 16 * 1024);
            let wire = reframe(&headers);
            assert_section_too_large(ConnectRequest::decode(&mut wire.as_slice()));
            assert_section_too_large(ConnectResponse::decode(&mut wire.as_slice()));
        }
    }

    #[tokio::test]
    async fn connect_rejects_declared_size_before_payload_arrives() {
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        VarInt::from_u32(16 * 1024 + 1).encode(&mut wire);
        assert_section_too_large(ConnectRequest::decode(&mut wire.as_slice()));
        assert_section_too_large(ConnectResponse::decode(&mut wire.as_slice()));
        assert_section_too_large(ConnectRequest::read(&mut Cursor::new(wire.clone())).await);
        assert_section_too_large(ConnectResponse::read(&mut Cursor::new(wire)).await);
    }

    #[test]
    fn response_charges_duplicate_literal_names() {
        let mut headers = payload(&encode_response());
        for _ in 0..500 {
            append_literal(&mut headers, b"x", b"");
        }
        assert_section_too_large(ConnectResponse::decode(&mut reframe(&headers).as_slice()));
    }

    #[test]
    fn connect_rejects_huffman_name_expansion() {
        let mut headers = payload(&encode_request("https://example.com/"));
        let name = huffman_a(17_000);
        qpack::encode_prefix(&mut headers, 3, 0b00101, name.len());
        headers.extend_from_slice(&name);
        qpack::encode_prefix(&mut headers, 7, 0, 0);
        assert!(headers.len() < MAX_FIELD_SECTION_SIZE as usize);
        let wire = reframe(&headers);
        assert_section_too_large(ConnectRequest::decode(&mut wire.as_slice()));
        assert_section_too_large(ConnectResponse::decode(&mut wire.as_slice()));
    }

    #[tokio::test]
    async fn grease_keeps_its_frame_budget_and_skips_iteratively() {
        let mut grease = encode_grease_frame(&vec![0; 20 * 1024]);
        for _ in 0..10_000 {
            grease.extend_from_slice(&encode_grease_frame(b""));
        }
        for base in [encode_request("https://example.com/"), encode_response()] {
            let mut wire = grease.clone();
            wire.extend_from_slice(&base);
            if base == encode_response() {
                assert_eq!(
                    ConnectResponse::decode(&mut wire.as_slice())
                        .unwrap()
                        .status,
                    http::StatusCode::OK
                );
                assert_eq!(
                    ConnectResponse::read(&mut Cursor::new(wire))
                        .await
                        .unwrap()
                        .status,
                    http::StatusCode::OK
                );
            } else {
                assert_eq!(
                    ConnectRequest::decode(&mut wire.as_slice())
                        .unwrap()
                        .url
                        .as_str(),
                    "https://example.com/"
                );
                assert_eq!(
                    ConnectRequest::read(&mut Cursor::new(wire))
                        .await
                        .unwrap()
                        .url
                        .as_str(),
                    "https://example.com/"
                );
            }
        }
        let wire = encode_grease_frame(&vec![0; MAX_FRAME_SIZE as usize + 1]);
        assert!(matches!(
            ConnectRequest::decode(&mut wire.as_slice()),
            Err(ConnectError::FrameTooLarge)
        ));
        assert!(matches!(
            ConnectResponse::read(&mut Cursor::new(wire)).await,
            Err(ConnectError::FrameTooLarge)
        ));
    }

    #[test]
    fn declared_size_is_checked_without_pointer_width_truncation() {
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        VarInt::try_from((1_u64 << 32) + 2)
            .unwrap()
            .encode(&mut wire);
        wire.extend_from_slice(&[0, 0]);
        assert_section_too_large(ConnectRequest::decode(&mut wire.as_slice()));
        assert_section_too_large(ConnectResponse::decode(&mut wire.as_slice()));
    }

    // ---- ConnectRequest::read tests ----

    #[tokio::test]
    async fn request_read_exact_consumption() {
        // Verify read() consumes only the frame bytes, not trailing data.
        let mut wire = encode_request("https://example.com/path");
        let trailing = b"trailing data";
        wire.extend_from_slice(trailing);

        let mut cursor = Cursor::new(wire);
        let req = ConnectRequest::read(&mut cursor).await.unwrap();
        assert_eq!(req.url.as_str(), "https://example.com/path");

        // The cursor should be positioned right after the frame, leaving trailing data.
        let pos = cursor.position() as usize;
        let remaining = &cursor.into_inner()[pos..];
        assert_eq!(remaining, trailing);
    }

    #[tokio::test]
    async fn request_read_roundtrip() {
        let wire = encode_request("https://example.com/foo?bar=1");
        let mut cursor = Cursor::new(wire);
        let req = ConnectRequest::read(&mut cursor).await.unwrap();
        assert_eq!(req.url.as_str(), "https://example.com/foo?bar=1");
    }

    #[tokio::test]
    async fn request_read_skips_grease() {
        // Prepend a GREASE frame before the real HEADERS frame.
        let mut wire = encode_grease_frame(b"junk");
        wire.extend_from_slice(&encode_request("https://example.com/"));

        let mut cursor = Cursor::new(wire);
        let req = ConnectRequest::read(&mut cursor).await.unwrap();
        assert_eq!(req.url.as_str(), "https://example.com/");
    }

    #[tokio::test]
    async fn request_read_rejects_frame_too_large() {
        // Craft a frame header claiming a huge payload.
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        // Refuse the HEADERS length before reading any payload.
        VarInt::from_u32(128 * 1024).encode(&mut wire);

        let mut cursor = Cursor::new(wire);
        let err = ConnectRequest::read(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, ConnectError::FieldSectionTooLarge),
            "expected FieldSectionTooLarge, got {err:?}"
        );
    }

    #[tokio::test]
    async fn request_read_rejects_wrong_frame_type() {
        let mut wire = Vec::new();
        Frame::DATA.encode(&mut wire);
        VarInt::from_u32(0).encode(&mut wire);

        let mut cursor = Cursor::new(wire);
        let err = ConnectRequest::read(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, ConnectError::UnexpectedFrame(f) if f == Frame::DATA),
            "expected UnexpectedFrame(DATA), got {err:?}"
        );
    }

    #[tokio::test]
    async fn request_read_empty_stream() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let err = ConnectRequest::read(&mut cursor).await.unwrap_err();
        assert!(matches!(err, ConnectError::UnexpectedEnd));
    }

    // ---- ConnectResponse::read tests ----

    #[tokio::test]
    async fn response_read_exact_consumption() {
        let mut wire = encode_response();
        let trailing = b"extra bytes";
        wire.extend_from_slice(trailing);

        let mut cursor = Cursor::new(wire);
        let resp = ConnectResponse::read(&mut cursor).await.unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);

        let pos = cursor.position() as usize;
        let remaining = &cursor.into_inner()[pos..];
        assert_eq!(remaining, trailing);
    }

    #[tokio::test]
    async fn response_read_roundtrip() {
        let wire = encode_response();
        let mut cursor = Cursor::new(wire);
        let resp = ConnectResponse::read(&mut cursor).await.unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
    }

    #[tokio::test]
    async fn response_read_skips_grease() {
        let mut wire = encode_grease_frame(b"grease");
        wire.extend_from_slice(&encode_response());

        let mut cursor = Cursor::new(wire);
        let resp = ConnectResponse::read(&mut cursor).await.unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
    }

    #[tokio::test]
    async fn response_read_rejects_frame_too_large() {
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        VarInt::from_u32(128 * 1024).encode(&mut wire);

        let mut cursor = Cursor::new(wire);
        let err = ConnectResponse::read(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, ConnectError::FieldSectionTooLarge),
            "expected FieldSectionTooLarge, got {err:?}"
        );
    }

    #[tokio::test]
    async fn response_read_rejects_wrong_frame_type() {
        let mut wire = Vec::new();
        Frame::SETTINGS.encode(&mut wire);
        VarInt::from_u32(0).encode(&mut wire);

        let mut cursor = Cursor::new(wire);
        let err = ConnectResponse::read(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, ConnectError::UnexpectedFrame(f) if f == Frame::SETTINGS),
            "expected UnexpectedFrame(SETTINGS), got {err:?}"
        );
    }

    #[tokio::test]
    async fn response_read_empty_stream() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let err = ConnectResponse::read(&mut cursor).await.unwrap_err();
        assert!(matches!(err, ConnectError::UnexpectedEnd));
    }

    // ---- Truncated payload tests ----

    #[tokio::test]
    async fn request_read_truncated_payload() {
        // Frame header claims 100 bytes but only 5 are present.
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        VarInt::from_u32(100).encode(&mut wire);
        wire.extend_from_slice(b"short");

        let mut cursor = Cursor::new(wire);
        let err = ConnectRequest::read(&mut cursor).await.unwrap_err();
        assert!(matches!(err, ConnectError::UnexpectedEnd));
    }

    #[tokio::test]
    async fn response_read_truncated_payload() {
        let mut wire = Vec::new();
        Frame::HEADERS.encode(&mut wire);
        VarInt::from_u32(100).encode(&mut wire);
        wire.extend_from_slice(b"short");

        let mut cursor = Cursor::new(wire);
        let err = ConnectResponse::read(&mut cursor).await.unwrap_err();
        assert!(matches!(err, ConnectError::UnexpectedEnd));
    }

    #[tokio::test]
    async fn request_read_truncated_grease() {
        // GREASE frame claims 100 bytes but only 3 are present.
        let mut wire = Vec::new();
        VarInt::from_u32(0x21).encode(&mut wire);
        VarInt::from_u32(100).encode(&mut wire);
        wire.extend_from_slice(b"abc");

        let mut cursor = Cursor::new(wire);
        let err = ConnectRequest::read(&mut cursor).await.unwrap_err();
        assert!(matches!(err, ConnectError::UnexpectedEnd));
    }

    #[test]
    fn request_debug_keeps_url_without_credentials() {
        let req = ConnectRequest::new(Url::parse("https://example.com:4443/moq/room").unwrap())
            .with_protocol("moq-lite-03");
        assert_eq!(
            format!("{req:?}"),
            r#"ConnectRequest { url: "https://example.com:4443/moq/room", protocols: ["moq-lite-03"], headers: {} }"#
        );
    }

    #[test]
    fn request_debug_redacts_url_credentials() {
        let url = "https://alice:hunter2@example.com/moq?jwt=SECRET&x=1#access_token=SECRET";
        let req = ConnectRequest::new(Url::parse(url).unwrap());
        let debug = format!("{req:?}");
        assert!(
            debug.contains(r#"url: "https://[redacted]@example.com/moq?[redacted]#[redacted]""#),
            "{debug}"
        );
        for secret in ["alice", "hunter2", "SECRET", "x=1"] {
            assert!(!debug.contains(secret), "{secret} leaked: {debug}");
        }

        let req = ConnectRequest::new(Url::parse("https://alice@example.com/").unwrap());
        assert!(format!("{req:?}").contains(r#""https://[redacted]@example.com/""#));
    }

    #[test]
    fn request_debug_redacts_header_values() {
        use http::{header, HeaderName, HeaderValue};

        let mut sensitive_agent = HeaderValue::from_static("agent-SECRET");
        sensitive_agent.set_sensitive(true);

        let req = ConnectRequest::new(Url::parse("https://example.com/").unwrap())
            .with_header(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer SECRET"),
            )
            .with_header(
                header::PROXY_AUTHORIZATION,
                HeaderValue::from_static("Basic SECRET"),
            )
            .with_header(header::COOKIE, HeaderValue::from_static("session=SECRET"))
            .with_header(
                header::SET_COOKIE,
                HeaderValue::from_static("session=SECRET"),
            )
            .with_header(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_static("SECRET"),
            )
            .with_header(
                header::ORIGIN,
                HeaderValue::from_static("https://app.example"),
            )
            .with_header(header::USER_AGENT, sensitive_agent);
        let debug = format!("{req:?}");

        assert!(!debug.contains("SECRET"), "{debug}");
        for name in [
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
            "x-api-key",
        ] {
            assert!(
                debug.contains(&format!(r#""{name}": [redacted]"#)),
                "{name}: {debug}"
            );
        }
        assert!(
            debug.contains(r#""origin": "https://app.example""#),
            "{debug}"
        );
        assert!(debug.contains(r#""user-agent": [redacted]"#), "{debug}");
    }

    #[test]
    fn decoded_request_debug_redacts_credentials() {
        let req = ConnectRequest::new(Url::parse("https://example.com/moq?jwt=SECRET").unwrap())
            .with_header(
                http::header::AUTHORIZATION,
                http::HeaderValue::from_static("Bearer SECRET"),
            )
            .with_header(
                http::header::USER_AGENT,
                http::HeaderValue::from_static("moq-test"),
            );
        let mut wire = Vec::new();
        req.encode(&mut wire).unwrap();

        let decoded = ConnectRequest::decode(&mut wire.as_slice()).unwrap();
        assert_eq!(decoded.url.query(), Some("jwt=SECRET"));

        // Decoded header order is unspecified, so check each entry separately.
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("SECRET"), "{debug}");
        for expected in [
            r#"url: "https://example.com/moq?[redacted]""#,
            r#""authorization": [redacted]"#,
            r#""user-agent": "moq-test""#,
        ] {
            assert!(debug.contains(expected), "{expected}: {debug}");
        }
    }
}
