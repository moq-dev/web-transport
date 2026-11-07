// This is a minimal QPACK implementation that only supports the static table and literals.
// By refusing to acknowledge the QPACK encoder, we can avoid implementing the dynamic table altogether.
// This is not recommended for a full HTTP/3 implementation but it's literally more efficient for handling a single WebTransport CONNECT request.

use std::collections::HashMap;

use bytes::{Buf, BufMut};

use super::huffman::{self, HpackStringDecode};
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum DecodeError {
    #[error("unexpected end of input")]
    UnexpectedEnd,

    #[error("varint bounds exceeded")]
    BoundsExceeded,

    #[error("dynamic references not supported")]
    DynamicEntry,

    #[error("unknown entry")]
    UnknownEntry,

    #[error("huffman decoding error")]
    HuffmanError(#[from] huffman::Error),

    #[error("invalid utf8 header")] // technically not required by the HTTP spec
    Utf8Error(#[from] std::str::Utf8Error),
}

#[derive(Debug, Error)]
pub(crate) enum FieldSectionError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("field section too large")]
    TooLarge,
}

// RFC 9114 section 4.2.2: each field line costs name + value + 32 bytes.
const FIELD_LINE_OVERHEAD: usize = 32;

struct Budget(usize);

impl Budget {
    fn new() -> Self {
        Self(crate::MAX_FIELD_SECTION_SIZE as usize)
    }

    fn charge(&mut self, bytes: usize) -> Result<(), FieldSectionError> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(FieldSectionError::TooLarge)?;
        Ok(())
    }
}

// Simple QPACK implementation that ONLY supports the static table and literals.
#[derive(Debug, Default)]
pub struct Headers {
    pub fields: HashMap<String, String>,
}

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(|v| v.as_str())
    }

    pub fn set(&mut self, name: &str, value: &str) {
        self.fields.insert(name.to_string(), value.to_string());
    }

    pub(crate) fn decode<B: Buf>(mut buf: &mut B) -> Result<Self, FieldSectionError> {
        let mut budget = Budget::new();
        // We don't support dynamic entries so we can skip these.
        let (_, _insert_count) = decode_prefix(buf, 8)?;
        let (_sign, _delta_base) = decode_prefix(buf, 7)?;

        let mut fields = HashMap::new();
        while buf.has_remaining() {
            // Every field line counts, including one that overwrites a previous name.
            budget.charge(FIELD_LINE_OVERHEAD)?;
            // Read the first byte;
            let peek = buf.get_u8();

            // Read the byte again by chaining Bufs.
            let first = [peek];
            let mut chain = first.chain(buf);

            // See: https://www.rfc-editor.org/rfc/rfc9204.html#section-4.5.2
            // This is over-engineered, LUL
            let (name, value) = match peek & 0b1100_0000 {
                // Indexed line field from static table
                0b1100_0000 => Self::decode_index(&mut chain, &mut budget)?,

                // Indexed line field from dynamic table
                0b1000_0000 => return Err(DecodeError::DynamicEntry.into()),

                _ => match peek & 0b1101_0000 {
                    // Indexed with literal name ref from static table
                    0b0101_0000 => Self::decode_literal_value(&mut chain, &mut budget)?,

                    // Indexed with literal name ref from dynamic table
                    0b0100_0000 => return Err(DecodeError::DynamicEntry.into()),

                    // Literal
                    _ if peek & 0b1110_0000 == 0b0010_0000 => {
                        Self::decode_literal(&mut chain, &mut budget)?
                    }

                    _ => match peek & 0b1111_0000 {
                        // Indexed with post base
                        0b0001_0000 => return Err(DecodeError::DynamicEntry.into()),

                        // Indexed with post base name ref
                        0b0000_0000 => return Err(DecodeError::DynamicEntry.into()),

                        // ugh
                        _ => return Err(DecodeError::UnknownEntry.into()),
                    },
                },
            };

            fields.insert(name, value);

            // Get the buffer back.
            (_, buf) = chain.into_inner();
        }

        Ok(Self { fields })
    }

    fn decode_index<B: Buf>(
        buf: &mut B,
        budget: &mut Budget,
    ) -> Result<(String, String), FieldSectionError> {
        /*
            0   1   2   3   4   5   6   7
        +---+---+---+---+---+---+---+---+
        | 1 | 1 |      Index (6+)       |
        +---+---+-----------------------+
        */

        let (_, index) = decode_prefix(buf, 6)?;
        let (name, value) = StaticTable::get(index)?;
        budget.charge(name.len())?;
        budget.charge(value.len())?;
        Ok((name.to_string(), value.to_string()))
    }

    fn decode_literal_value<B: Buf>(
        buf: &mut B,
        budget: &mut Budget,
    ) -> Result<(String, String), FieldSectionError> {
        /*
          0   1   2   3   4   5   6   7
        +---+---+---+---+---+---+---+---+
        | 0 | 1 | N | 1 |Name Index (4+)|
        +---+---+---+---+---------------+
        | H |     Value Length (7+)     |
        +---+---------------------------+
        |  Value String (Length bytes)  |
        +-------------------------------+
        */

        let (_, name) = decode_prefix(buf, 4)?;
        let (name, _) = StaticTable::get(name)?;

        budget.charge(name.len())?;
        let value = decode_string(buf, 8, budget)?;
        Ok((name.to_string(), value))
    }

    fn decode_literal<B: Buf>(
        buf: &mut B,
        budget: &mut Budget,
    ) -> Result<(String, String), FieldSectionError> {
        /*
          0   1   2   3   4   5   6   7
        +---+---+---+---+---+---+---+---+
        | 0 | 0 | 1 | N | H |NameLen(3+)|
        +---+---+---+---+---+-----------+
        |  Name String (Length bytes)   |
        +---+---------------------------+
        | H |     Value Length (7+)     |
        +---+---------------------------+
        |  Value String (Length bytes)  |
        +-------------------------------+
        */

        let name = decode_string(buf, 4, budget)?;
        let value = decode_string(buf, 8, budget)?;
        Ok((name, value))
    }

    /// Measures the field section, refusing one the decoder would reject as too large.
    ///
    /// Without Huffman coding, each encoded field line is shorter than its decoded
    /// cost, so the encoded section also fits the limit.
    pub(crate) fn encoded_size(&self) -> Result<usize, FieldSectionError> {
        let mut budget = Budget::new();
        let mut size = 2; // Required Insert Count and Delta Base.
        for (name, value) in &self.fields {
            budget.charge(FIELD_LINE_OVERHEAD)?;
            budget.charge(name.len())?;
            budget.charge(value.len())?;

            size += Self::field_parts(name, value)
                .iter()
                .map(Part::size)
                .sum::<usize>();
        }
        Ok(size)
    }

    /// Encodes a field section validated by `encoded_size`.
    pub(crate) fn encode<B: BufMut>(&self, buf: &mut B) {
        // We don't support dynamic entries so we can skip these.
        encode_prefix(buf, 8, 0, 0);
        encode_prefix(buf, 7, 0, 0);

        // We must encode pseudo-headers first.
        // https://datatracker.ietf.org/doc/html/rfc9114#section-4.1.2
        let headers = self
            .fields
            .iter()
            .filter(|(key, _)| key.starts_with(':'))
            .chain(self.fields.iter().filter(|(key, _)| !key.starts_with(':')));

        for (name, value) in headers {
            for part in Self::field_parts(name, value) {
                part.encode(buf);
            }
        }
    }

    // RFC 9204 section 4.5.2: choose the representation once for measurement and output.
    fn field_parts<'a>(name: &'a str, value: &'a str) -> [Part<'a>; 4] {
        let empty = Part::Bytes(&[]);
        if let Some(index) = StaticTable::find(name, value) {
            [Part::Prefix(6, 0b11, index), empty, empty, empty]
        } else if let Some(index) = StaticTable::find_name(name) {
            [
                Part::Prefix(4, 0b0101, index),
                empty,
                Part::Prefix(7, 0, value.len()),
                Part::Bytes(value.as_bytes()),
            ]
        } else {
            [
                Part::Prefix(3, 0b00100, name.len()),
                Part::Bytes(name.as_bytes()),
                Part::Prefix(7, 0, value.len()),
                Part::Bytes(value.as_bytes()),
            ]
        }
    }
}

// Wire pieces shared by field-size measurement and encoding, without payload storage.
#[derive(Clone, Copy)]
enum Part<'a> {
    Prefix(u8, u8, usize),
    Bytes(&'a [u8]),
}

impl Part<'_> {
    fn size(&self) -> usize {
        match self {
            Self::Prefix(bits, _, value) => prefix_size(*bits, *value),
            Self::Bytes(bytes) => bytes.len(),
        }
    }

    fn encode<B: BufMut>(&self, buf: &mut B) {
        match self {
            Self::Prefix(bits, flags, value) => encode_prefix(buf, *bits, *flags, *value),
            Self::Bytes(bytes) => buf.put_slice(bytes),
        }
    }
}

// An integer that uses a fixed number of bits, otherwise a variable number of bytes if it's too large.
// https://www.rfc-editor.org/rfc/rfc7541#section-5.1

// Based on : https://github.com/hyperium/h3/blob/master/h3/src/qpack/prefix_int.rs
// License: MIT

pub fn decode_prefix<B: Buf>(buf: &mut B, size: u8) -> Result<(u8, usize), DecodeError> {
    assert!(size <= 8);

    if !buf.has_remaining() {
        return Err(DecodeError::UnexpectedEnd);
    }

    let mut first = buf.get_u8();

    // NOTE: following casts to u8 intend to trim the most significant bits, they are used as a
    //       workaround for shiftoverflow errors when size == 8.
    let flags = ((first as usize) >> size) as u8;
    let mask = 0xFF >> (8 - size);
    first &= mask;

    // if first < 2usize.pow(size) - 1
    if first < mask {
        return Ok((flags, first as usize));
    }

    let mut value = mask as usize;
    let mut power = 0usize;
    loop {
        if !buf.has_remaining() {
            return Err(DecodeError::UnexpectedEnd);
        }

        let byte = buf.get_u8() as usize;
        let part = byte & 127;
        if power >= usize::BITS as usize || part > usize::MAX >> power {
            return Err(DecodeError::BoundsExceeded);
        }
        value = value
            .checked_add(part << power)
            .ok_or(DecodeError::BoundsExceeded)?;
        power += 7;

        if byte & 128 == 0 {
            break;
        }

        if power >= usize::BITS as usize {
            return Err(DecodeError::BoundsExceeded);
        }
    }

    Ok((flags, value))
}

pub fn encode_prefix<B: BufMut>(buf: &mut B, size: u8, flags: u8, value: usize) {
    assert!(size > 0 && size <= 8);

    // NOTE: following casts to u8 intend to trim the most significant bits, they are used as a
    //       workaround for shiftoverflow errors when size == 8.
    let mask = !(0xFF << size) as u8;
    let flags = ((flags as usize) << size) as u8;

    // if value < 2usize.pow(size) - 1
    if value < (mask as usize) {
        buf.put_u8(flags | value as u8);
        return;
    }

    buf.put_u8(mask | flags);
    let mut remaining = value - mask as usize;

    while remaining >= 128 {
        let rest = (remaining % 128) as u8;
        buf.put_u8(rest + 128);
        remaining /= 128;
    }

    buf.put_u8(remaining as u8);
}

fn prefix_size(bits: u8, value: usize) -> usize {
    let mask = (1 << bits) - 1;
    if value < mask {
        return 1;
    }
    let mut remaining = value - mask;
    let mut size = 2;
    while remaining >= 128 {
        remaining /= 128;
        size += 1;
    }
    size
}

fn decode_string<B: Buf>(
    buf: &mut B,
    size: u8,
    budget: &mut Budget,
) -> Result<String, FieldSectionError> {
    if !buf.has_remaining() {
        return Err(DecodeError::UnexpectedEnd.into());
    }
    let (flags, len) = decode_prefix(buf, size - 1)?;
    if buf.remaining() < len {
        return Err(DecodeError::UnexpectedEnd.into());
    }

    let value = if flags & 1 == 0 {
        budget.charge(len)?;
        let mut value = vec![0; len];
        buf.copy_to_slice(&mut value);
        value
    } else if buf.chunk().len() >= len {
        let result = decode_huffman(&buf.chunk()[..len], budget);
        // Consume the encoded string even on a decoding or budget error.
        buf.advance(len);
        result?
    } else {
        decode_huffman(&buf.copy_to_bytes(len), budget)?
    };
    String::from_utf8(value).map_err(|err| DecodeError::Utf8Error(err.utf8_error()).into())
}

fn decode_huffman(encoded: &[u8], budget: &mut Budget) -> Result<Vec<u8>, FieldSectionError> {
    let mut decoded = Vec::new();
    for byte in encoded.hpack_decode() {
        let byte = byte.map_err(DecodeError::from)?;
        budget.charge(1)?;
        decoded.push(byte);
    }
    Ok(decoded)
}

// Based on https://github.com/hyperium/h3/blob/master/h3/src/qpack/static_.rs
// I switched over to str because it's nicer in Rust... even though HTTP doesn't use utf8.
struct StaticTable {}

impl StaticTable {
    pub fn get(index: usize) -> Result<(&'static str, &'static str), DecodeError> {
        match PREDEFINED_HEADERS.get(index) {
            Some(v) => Ok(*v),
            None => Err(DecodeError::UnknownEntry),
        }
    }

    // TODO combine find and find_name to do a single lookup
    pub fn find(name: &str, value: &str) -> Option<usize> {
        match (name, value) {
            (":authority", "") => Some(0),
            (":path", "/") => Some(1),
            ("age", "0") => Some(2),
            ("content-disposition", "") => Some(3),
            ("content-length", "0") => Some(4),
            ("cookie", "") => Some(5),
            ("date", "") => Some(6),
            ("etag", "") => Some(7),
            ("if-modified-since", "") => Some(8),
            ("if-none-match", "") => Some(9),
            ("last-modified", "") => Some(10),
            ("link", "") => Some(11),
            ("location", "") => Some(12),
            ("referer", "") => Some(13),
            ("set-cookie", "") => Some(14),
            (":method", "CONNECT") => Some(15),
            (":method", "DELETE") => Some(16),
            (":method", "GET") => Some(17),
            (":method", "HEAD") => Some(18),
            (":method", "OPTIONS") => Some(19),
            (":method", "POST") => Some(20),
            (":method", "PUT") => Some(21),
            (":scheme", "http") => Some(22),
            (":scheme", "https") => Some(23),
            (":status", "103") => Some(24),
            (":status", "200") => Some(25),
            (":status", "304") => Some(26),
            (":status", "404") => Some(27),
            (":status", "503") => Some(28),
            ("accept", "*/*") => Some(29),
            ("accept", "application/dns-message") => Some(30),
            ("accept-encoding", "gzip, deflate, br") => Some(31),
            ("accept-ranges", "bytes") => Some(32),
            ("access-control-allow-headers", "cache-control") => Some(33),
            ("access-control-allow-headers", "content-type") => Some(34),
            ("access-control-allow-origin", "*") => Some(35),
            ("cache-control", "max-age=0") => Some(36),
            ("cache-control", "max-age=2592000") => Some(37),
            ("cache-control", "max-age=604800") => Some(38),
            ("cache-control", "no-cache") => Some(39),
            ("cache-control", "no-store") => Some(40),
            ("cache-control", "public, max-age=31536000") => Some(41),
            ("content-encoding", "br") => Some(42),
            ("content-encoding", "gzip") => Some(43),
            ("content-type", "application/dns-message") => Some(44),
            ("content-type", "application/javascript") => Some(45),
            ("content-type", "application/json") => Some(46),
            ("content-type", "application/x-www-form-urlencoded") => Some(47),
            ("content-type", "image/gif") => Some(48),
            ("content-type", "image/jpeg") => Some(49),
            ("content-type", "image/png") => Some(50),
            ("content-type", "text/css") => Some(51),
            ("content-type", "text/html; charset=utf-8") => Some(52),
            ("content-type", "text/plain") => Some(53),
            ("content-type", "text/plain;charset=utf-8") => Some(54),
            ("range", "bytes=0-") => Some(55),
            ("strict-transport-security", "max-age=31536000") => Some(56),
            ("strict-transport-security", "max-age=31536000; includesubdomains") => Some(57),
            ("strict-transport-security", "max-age=31536000; includesubdomains; preload") => {
                Some(58)
            }
            ("vary", "accept-encoding") => Some(59),
            ("vary", "origin") => Some(60),
            ("x-content-type-options", "nosniff") => Some(61),
            ("x-xss-protection", "1; mode=block") => Some(62),
            (":status", "100") => Some(63),
            (":status", "204") => Some(64),
            (":status", "206") => Some(65),
            (":status", "302") => Some(66),
            (":status", "400") => Some(67),
            (":status", "403") => Some(68),
            (":status", "421") => Some(69),
            (":status", "425") => Some(70),
            (":status", "500") => Some(71),
            ("accept-language", "") => Some(72),
            ("access-control-allow-credentials", "FALSE") => Some(73),
            ("access-control-allow-credentials", "TRUE") => Some(74),
            ("access-control-allow-headers", "*") => Some(75),
            ("access-control-allow-methods", "get") => Some(76),
            ("access-control-allow-methods", "get, post, options") => Some(77),
            ("access-control-allow-methods", "options") => Some(78),
            ("access-control-expose-headers", "content-length") => Some(79),
            ("access-control-request-headers", "content-type") => Some(80),
            ("access-control-request-method", "get") => Some(81),
            ("access-control-request-method", "post") => Some(82),
            ("alt-svc", "clear") => Some(83),
            ("authorization", "") => Some(84),
            (
                "content-security-policy",
                "script-src 'none'; object-src 'none'; base-uri 'none'",
            ) => Some(85),
            ("early-data", "1") => Some(86),
            ("expect-ct", "") => Some(87),
            ("forwarded", "") => Some(88),
            ("if-range", "") => Some(89),
            ("origin", "") => Some(90),
            ("purpose", "prefetch") => Some(91),
            ("server", "") => Some(92),
            ("timing-allow-origin", "*") => Some(93),
            ("upgrade-insecure-requests", "1") => Some(94),
            ("user-agent", "") => Some(95),
            ("x-forwarded-for", "") => Some(96),
            ("x-frame-options", "deny") => Some(97),
            ("x-frame-options", "sameorigin") => Some(98),
            _ => None,
        }
    }

    pub fn find_name(name: &str) -> Option<usize> {
        match name {
            ":authority" => Some(0),
            ":path" => Some(1),
            "age" => Some(2),
            "content-disposition" => Some(3),
            "content-length" => Some(4),
            "cookie" => Some(5),
            "date" => Some(6),
            "etag" => Some(7),
            "if-modified-since" => Some(8),
            "if-none-match" => Some(9),
            "last-modified" => Some(10),
            "link" => Some(11),
            "location" => Some(12),
            "referer" => Some(13),
            "set-cookie" => Some(14),
            ":method" => Some(15),
            ":scheme" => Some(22),
            ":status" => Some(24),
            "accept" => Some(29),
            "accept-encoding" => Some(31),
            "accept-ranges" => Some(32),
            "access-control-allow-headers" => Some(33),
            "access-control-allow-origin" => Some(35),
            "cache-control" => Some(36),
            "content-encoding" => Some(42),
            "content-type" => Some(44),
            "range" => Some(55),
            "strict-transport-security" => Some(56),
            "vary" => Some(59),
            "x-content-type-options" => Some(61),
            "x-xss-protection" => Some(62),
            "accept-language" => Some(72),
            "access-control-allow-credentials" => Some(73),
            "access-control-allow-methods" => Some(76),
            "access-control-expose-headers" => Some(79),
            "access-control-request-headers" => Some(80),
            "access-control-request-method" => Some(81),
            "alt-svc" => Some(83),
            "authorization" => Some(84),
            "content-security-policy" => Some(85),
            "early-data" => Some(86),
            "expect-ct" => Some(87),
            "forwarded" => Some(88),
            "if-range" => Some(89),
            "origin" => Some(90),
            "purpose" => Some(91),
            "server" => Some(92),
            "timing-allow-origin" => Some(93),
            "upgrade-insecure-requests" => Some(94),
            "user-agent" => Some(95),
            "x-forwarded-for" => Some(96),
            "x-frame-options" => Some(97),
            _ => None,
        }
    }
}

const PREDEFINED_HEADERS: [(&str, &str); 99] = [
    (":authority", ""),
    (":path", "/"),
    ("age", "0"),
    ("content-disposition", ""),
    ("content-length", "0"),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("referer", ""),
    ("set-cookie", ""),
    (":method", "CONNECT"),
    (":method", "DELETE"),
    (":method", "GET"),
    (":method", "HEAD"),
    (":method", "OPTIONS"),
    (":method", "POST"),
    (":method", "PUT"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "103"),
    (":status", "200"),
    (":status", "304"),
    (":status", "404"),
    (":status", "503"),
    ("accept", "*/*"),
    ("accept", "application/dns-message"),
    ("accept-encoding", "gzip, deflate, br"),
    ("accept-ranges", "bytes"),
    ("access-control-allow-headers", "cache-control"),
    ("access-control-allow-headers", "content-type"),
    ("access-control-allow-origin", "*"),
    ("cache-control", "max-age=0"),
    ("cache-control", "max-age=2592000"),
    ("cache-control", "max-age=604800"),
    ("cache-control", "no-cache"),
    ("cache-control", "no-store"),
    ("cache-control", "public, max-age=31536000"),
    ("content-encoding", "br"),
    ("content-encoding", "gzip"),
    ("content-type", "application/dns-message"),
    ("content-type", "application/javascript"),
    ("content-type", "application/json"),
    ("content-type", "application/x-www-form-urlencoded"),
    ("content-type", "image/gif"),
    ("content-type", "image/jpeg"),
    ("content-type", "image/png"),
    ("content-type", "text/css"),
    ("content-type", "text/html; charset=utf-8"),
    ("content-type", "text/plain"),
    ("content-type", "text/plain;charset=utf-8"),
    ("range", "bytes=0-"),
    ("strict-transport-security", "max-age=31536000"),
    (
        "strict-transport-security",
        "max-age=31536000; includesubdomains",
    ),
    (
        "strict-transport-security",
        "max-age=31536000; includesubdomains; preload",
    ),
    ("vary", "accept-encoding"),
    ("vary", "origin"),
    ("x-content-type-options", "nosniff"),
    ("x-xss-protection", "1; mode=block"),
    (":status", "100"),
    (":status", "204"),
    (":status", "206"),
    (":status", "302"),
    (":status", "400"),
    (":status", "403"),
    (":status", "421"),
    (":status", "425"),
    (":status", "500"),
    ("accept-language", ""),
    ("access-control-allow-credentials", "FALSE"),
    ("access-control-allow-credentials", "TRUE"),
    ("access-control-allow-headers", "*"),
    ("access-control-allow-methods", "get"),
    ("access-control-allow-methods", "get, post, options"),
    ("access-control-allow-methods", "options"),
    ("access-control-expose-headers", "content-length"),
    ("access-control-request-headers", "content-type"),
    ("access-control-request-method", "get"),
    ("access-control-request-method", "post"),
    ("alt-svc", "clear"),
    ("authorization", ""),
    (
        "content-security-policy",
        "script-src 'none'; object-src 'none'; base-uri 'none'",
    ),
    ("early-data", "1"),
    ("expect-ct", ""),
    ("forwarded", ""),
    ("if-range", ""),
    ("origin", ""),
    ("purpose", "prefetch"),
    ("server", ""),
    ("timing-allow-origin", "*"),
    ("upgrade-insecure-requests", "1"),
    ("user-agent", ""),
    ("x-forwarded-for", ""),
    ("x-frame-options", "deny"),
    ("x-frame-options", "sameorigin"),
];

#[cfg(test)]
mod tests {
    use super::*;

    // Split every byte to exercise string copying across arbitrary Buf chunks.
    struct Chunks<'a>(&'a [u8]);
    impl Buf for Chunks<'_> {
        fn remaining(&self) -> usize {
            self.0.len()
        }
        fn chunk(&self) -> &[u8] {
            &self.0[..self.0.len().min(1)]
        }
        fn advance(&mut self, cnt: usize) {
            self.0 = &self.0[cnt..];
        }
    }

    #[test]
    fn measured_prefixes_match_wire_at_width_boundaries() {
        for bits in 1..=8 {
            let mask = (1 << bits) - 1;
            for value in [
                0,
                mask - 1,
                mask,
                mask + 1,
                mask + 127,
                mask + 128,
                mask + 16383,
                mask + 16384,
                usize::MAX,
            ] {
                let mut wire = Vec::new();
                encode_prefix(&mut wire, bits, 0, value);
                assert_eq!(prefix_size(bits, value), wire.len());
                assert_eq!(decode_prefix(&mut wire.as_slice(), bits).unwrap().1, value);
            }
        }
    }

    #[test]
    fn measured_sections_cover_each_field_representation() {
        for len in [0, 6, 7, 126, 127, 128, 254, 255, 256, 4096] {
            let mut headers = Headers::default();
            headers.set("x-first", "ordinary");
            headers.set(":status", "200"); // Full static index.
            headers.set("user-agent", &"a".repeat(len)); // Static name index > 15.
            headers.set(&"b".repeat(len), &"c".repeat(len)); // Literal name and value.
            let size = headers.encoded_size().unwrap();
            let mut wire = Vec::new();
            headers.encode(&mut wire);
            assert_eq!(size, wire.len());
            assert_eq!(&wire[..3], &[0, 0, 0xd9], "pseudo-header is first");

            let decoded = Headers::decode(&mut Chunks(&wire)).unwrap();
            assert_eq!(decoded.fields.len(), headers.fields.len());
            for (name, value) in &headers.fields {
                assert_eq!(decoded.get(name), Some(value.as_str()));
            }
        }
    }

    #[test]
    fn huffman_contiguous_and_fragmented_inputs_consume_the_same_string_on_error() {
        for padding in [0xff, 0xf8] {
            for budget in [1, 2] {
                // Two 'a' symbols and six padding bits, followed by another string.
                let wire = [0x82, 0x18, padding, 0x01, b'x'];
                let mut contiguous = wire.as_slice();
                let mut fragmented = Chunks(&wire);
                let result = decode_string(&mut contiguous, 8, &mut Budget(budget));
                let other = decode_string(&mut fragmented, 8, &mut Budget(budget));
                if padding == 0xff && budget == 2 {
                    assert_eq!(result.unwrap(), "aa");
                    assert_eq!(other.unwrap(), "aa");
                } else {
                    assert_eq!(
                        result.unwrap_err().to_string(),
                        other.unwrap_err().to_string()
                    );
                }
                assert_eq!(contiguous, &wire[3..]);
                assert_eq!(fragmented.0, &wire[3..]);
            }
        }
    }

    #[test]
    fn strings_charge_before_output_and_validate_at_exact_budget() {
        let mut plain = Vec::new();
        encode_prefix(&mut plain, 7, 0, 4);
        plain.extend_from_slice(b"text");
        let mut input = plain.as_slice();
        assert!(matches!(
            decode_string(&mut input, 8, &mut Budget(3)),
            Err(FieldSectionError::TooLarge)
        ));
        assert_eq!(input, b"text", "raw payload was not copied before charging");

        // One 'a' plus legal three-bit padding, then invalid zero padding.
        assert_eq!(
            decode_string(&mut &[0x81, 0x1f][..], 8, &mut Budget(1)).unwrap(),
            "a"
        );
        assert!(matches!(
            decode_string(&mut &[0x81, 0x18][..], 8, &mut Budget(1)),
            Err(FieldSectionError::Decode(DecodeError::HuffmanError(_)))
        ));
        assert_eq!(
            decode_string(&mut &[0x80][..], 8, &mut Budget(0)).unwrap(),
            ""
        );
    }

    #[test]
    fn exact_field_budget_accepts_empty_huffman_value() {
        let mut wire = vec![0, 0];
        let name = "a".repeat(crate::MAX_FIELD_SECTION_SIZE as usize - 32);
        encode_prefix(&mut wire, 3, 0b00100, name.len());
        wire.extend_from_slice(name.as_bytes());
        encode_prefix(&mut wire, 7, 1, 0);
        let headers = Headers::decode(&mut wire.as_slice()).unwrap();
        assert_eq!(headers.get(&name), Some(""));
        // A second occurrence still costs its own 32-byte overhead.
        for part in Headers::field_parts("x", "") {
            part.encode(&mut wire);
        }
        assert!(matches!(
            Headers::decode(&mut wire.as_slice()),
            Err(FieldSectionError::TooLarge)
        ));
    }

    #[test]
    fn prefixed_integer_refuses_shift_and_sum_overflow() {
        let mut maximum = Vec::new();
        encode_prefix(&mut maximum, 8, 0, usize::MAX);
        assert_eq!(
            decode_prefix(&mut maximum.as_slice(), 8).unwrap().1,
            usize::MAX
        );
        *maximum.last_mut().unwrap() += 1;
        assert!(matches!(
            decode_prefix(&mut maximum.as_slice(), 8),
            Err(DecodeError::BoundsExceeded)
        ));

        let groups = usize::BITS.div_ceil(7) as usize;
        let mut sum_overflow = vec![0xff; groups];
        sum_overflow.push(((1_usize << ((usize::BITS - 1) % 7 + 1)) - 1) as u8);
        assert!(matches!(
            decode_prefix(&mut sum_overflow.as_slice(), 8),
            Err(DecodeError::BoundsExceeded)
        ));
    }
}
