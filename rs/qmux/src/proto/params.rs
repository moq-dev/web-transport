use bytes::{Buf, BufMut, Bytes, BytesMut};
use web_transport_proto::VarInt;

use crate::Error;

/// Transport parameters exchanged during QMux connection setup.
///
/// These mirror the QUIC transport parameters from RFC 9000, Section 18.2,
/// plus QMux-specific extensions from draft-01.
/// All values default to 0 (per QUIC), meaning no data/streams allowed
/// until the peer advertises its limits.
#[derive(Debug, Clone, Default)]
pub struct TransportParams {
    pub max_idle_timeout: u64,                    // ID 0x01 (milliseconds)
    pub initial_max_data: u64,                    // ID 0x04
    pub initial_max_stream_data_bidi_local: u64,  // ID 0x05
    pub initial_max_stream_data_bidi_remote: u64, // ID 0x06
    pub initial_max_stream_data_uni: u64,         // ID 0x07
    pub initial_max_streams_bidi: u64,            // ID 0x08
    pub initial_max_streams_uni: u64,             // ID 0x09
    pub max_record_size: u64,                     // ID 0x0571c59429cd0845 (default 16382)

    /// Application protocols advertised for negotiation (preference order).
    ///
    /// QMux-specific (ID 0x3d4f9c2a8b1e6075). This is the non-TLS substitute
    /// for ALPN: each side lists the protocols it supports and both derive the
    /// agreed protocol deterministically (server preference wins). Empty when
    /// the application protocol was negotiated out of band (e.g. via TLS/WS
    /// ALPN) or not negotiated at all; the parameter is then omitted entirely.
    pub protocols: Vec<String>,

    /// Requested resource path, for transports without a URL of their own.
    ///
    /// QMux-specific (ID 0x2b9a4c1f6e3d8052). WebTransport over HTTP/3 and
    /// WebSocket carry the path in the request line (`:path` / the WS URL);
    /// TCP, TLS, and Unix sockets have no such field, so a client that needs to
    /// address a specific resource sends it here. `None` (the default) omits the
    /// parameter entirely, keeping peers that never set a path byte-identical.
    pub path: Option<String>,
}

/// Default max_record_size per draft-01.
pub const DEFAULT_MAX_RECORD_SIZE: u64 = 16382;

// max_record_size parameter ID (QMux-specific, exceeds u32)
const MAX_RECORD_SIZE_ID: u64 = 0x0571c59429cd0845;
// SAFETY: 0x0571c59429cd0845 < 2^62 (VarInt max)
const MAX_RECORD_SIZE_ID_VI: VarInt = unsafe { VarInt::from_u64_unchecked(MAX_RECORD_SIZE_ID) };
const _: () = assert!(
    MAX_RECORD_SIZE_ID < (1 << 62),
    "MAX_RECORD_SIZE_ID must fit in VarInt"
);

// application_protocols parameter ID (QMux-specific, exceeds u32).
// Not part of QUIC v1; carries the ALPN list on transports without TLS.
const APPLICATION_PROTOCOLS_ID: u64 = 0x3d4f9c2a8b1e6075;
// SAFETY: 0x3d4f9c2a8b1e6075 < 2^62 (VarInt max)
const APPLICATION_PROTOCOLS_ID_VI: VarInt =
    unsafe { VarInt::from_u64_unchecked(APPLICATION_PROTOCOLS_ID) };
const _: () = assert!(
    APPLICATION_PROTOCOLS_ID < (1 << 62),
    "APPLICATION_PROTOCOLS_ID must fit in VarInt"
);

// path parameter ID (QMux-specific). Carries the requested resource path on
// transports that lack a request line of their own (TCP, TLS, Unix sockets).
const PATH_ID: u64 = 0x2b9a4c1f6e3d8052;
// SAFETY: 0x2b9a4c1f6e3d8052 < 2^62 (VarInt max)
const PATH_ID_VI: VarInt = unsafe { VarInt::from_u64_unchecked(PATH_ID) };
const _: () = assert!(PATH_ID < (1 << 62), "PATH_ID must fit in VarInt");

impl TransportParams {
    // Transport parameter IDs
    const MAX_IDLE_TIMEOUT: VarInt = VarInt::from_u32(0x01);
    const INITIAL_MAX_DATA: VarInt = VarInt::from_u32(0x04);
    const INITIAL_MAX_STREAM_DATA_BIDI_LOCAL: VarInt = VarInt::from_u32(0x05);
    const INITIAL_MAX_STREAM_DATA_BIDI_REMOTE: VarInt = VarInt::from_u32(0x06);
    const INITIAL_MAX_STREAM_DATA_UNI: VarInt = VarInt::from_u32(0x07);
    const INITIAL_MAX_STREAMS_BIDI: VarInt = VarInt::from_u32(0x08);
    const INITIAL_MAX_STREAMS_UNI: VarInt = VarInt::from_u32(0x09);

    /// Encode transport parameters as a series of ID-length-value tuples.
    pub fn encode(&self) -> Result<Bytes, Error> {
        let mut buf = BytesMut::new();

        fn write_param(buf: &mut BytesMut, id: VarInt, value: u64) -> Result<(), Error> {
            if value == 0 {
                return Ok(());
            }
            let val_vi = VarInt::try_from(value)?;
            let val_size = varint_size(value);

            id.encode(buf);
            VarInt::from_u32(val_size as u32).encode(buf);
            val_vi.encode(buf);
            Ok(())
        }

        write_param(&mut buf, Self::MAX_IDLE_TIMEOUT, self.max_idle_timeout)?;
        write_param(&mut buf, Self::INITIAL_MAX_DATA, self.initial_max_data)?;
        write_param(
            &mut buf,
            Self::INITIAL_MAX_STREAM_DATA_BIDI_LOCAL,
            self.initial_max_stream_data_bidi_local,
        )?;
        write_param(
            &mut buf,
            Self::INITIAL_MAX_STREAM_DATA_BIDI_REMOTE,
            self.initial_max_stream_data_bidi_remote,
        )?;
        write_param(
            &mut buf,
            Self::INITIAL_MAX_STREAM_DATA_UNI,
            self.initial_max_stream_data_uni,
        )?;
        write_param(
            &mut buf,
            Self::INITIAL_MAX_STREAMS_BIDI,
            self.initial_max_streams_bidi,
        )?;
        write_param(
            &mut buf,
            Self::INITIAL_MAX_STREAMS_UNI,
            self.initial_max_streams_uni,
        )?;
        write_param(&mut buf, MAX_RECORD_SIZE_ID_VI, self.max_record_size)?;

        // application_protocols: a list of length-prefixed UTF-8 names. Omitted
        // entirely when empty so peers that don't negotiate stay byte-identical.
        if !self.protocols.is_empty() {
            let mut value = BytesMut::new();
            for protocol in &self.protocols {
                VarInt::try_from(protocol.len())?.encode(&mut value);
                value.put_slice(protocol.as_bytes());
            }
            APPLICATION_PROTOCOLS_ID_VI.encode(&mut buf);
            VarInt::try_from(value.len())?.encode(&mut buf);
            buf.put_slice(&value);
        }

        // path: a single UTF-8 string. Omitted entirely when unset so peers that
        // don't address a resource stay byte-identical to the old format. An
        // explicitly empty path is a distinct, valid value and is still sent.
        if let Some(path) = &self.path {
            PATH_ID_VI.encode(&mut buf);
            VarInt::try_from(path.len())?.encode(&mut buf);
            buf.put_slice(path.as_bytes());
        }

        Ok(buf.freeze())
    }

    /// Decode transport parameters from bytes.
    pub fn decode(mut data: Bytes) -> Result<Self, Error> {
        // Per draft-01, `max_record_size` defaults to 16382 when omitted, not 0.
        let mut params = TransportParams {
            max_record_size: DEFAULT_MAX_RECORD_SIZE,
            ..TransportParams::default()
        };
        // Track seen IDs to detect duplicates using a set of seen IDs
        let mut seen = std::collections::HashSet::new();

        while data.has_remaining() {
            let id = VarInt::decode(&mut data)?.into_inner();
            let len = VarInt::decode(&mut data)?.into_inner() as usize;

            if data.remaining() < len {
                return Err(Error::Short);
            }

            let mut param_data = data.split_to(len);

            match id {
                APPLICATION_PROTOCOLS_ID => {
                    if !seen.insert(id) {
                        return Err(Error::DuplicateParam(id));
                    }
                    params.protocols = decode_protocols(&mut param_data)?;
                }
                PATH_ID => {
                    if !seen.insert(id) {
                        return Err(Error::DuplicateParam(id));
                    }
                    // The whole payload is the path; an empty payload is a valid
                    // (empty) path, distinct from the parameter being absent.
                    params.path = Some(
                        std::str::from_utf8(&param_data)
                            .map_err(|_| Error::InvalidPath(format!("{param_data:?}")))?
                            .to_string(),
                    );
                }
                0x01 | 0x04..=0x09 | MAX_RECORD_SIZE_ID => {
                    if !seen.insert(id) {
                        return Err(Error::DuplicateParam(id));
                    }

                    match id {
                        0x01 => params.max_idle_timeout = decode_varint_param(&mut param_data)?,
                        0x04 => params.initial_max_data = decode_varint_param(&mut param_data)?,
                        0x05 => {
                            params.initial_max_stream_data_bidi_local =
                                decode_varint_param(&mut param_data)?
                        }
                        0x06 => {
                            params.initial_max_stream_data_bidi_remote =
                                decode_varint_param(&mut param_data)?
                        }
                        0x07 => {
                            params.initial_max_stream_data_uni =
                                decode_varint_param(&mut param_data)?
                        }
                        0x08 => {
                            params.initial_max_streams_bidi = decode_varint_param(&mut param_data)?
                        }
                        0x09 => {
                            params.initial_max_streams_uni = decode_varint_param(&mut param_data)?
                        }
                        MAX_RECORD_SIZE_ID => {
                            params.max_record_size = decode_varint_param(&mut param_data)?
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {
                    // Unknown parameter, skip (already split off)
                }
            }
        }

        Ok(params)
    }
}

/// Decode the application_protocols value: a sequence of length-prefixed
/// UTF-8 protocol names that consumes the whole parameter payload.
fn decode_protocols(data: &mut Bytes) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    while data.has_remaining() {
        let len = VarInt::decode(data)?.into_inner() as usize;
        if data.remaining() < len {
            return Err(Error::Short);
        }
        let name = data.split_to(len);
        let protocol =
            std::str::from_utf8(&name).map_err(|_| Error::InvalidProtocol(format!("{name:?}")))?;
        out.push(protocol.to_string());
    }
    // The encoder omits the parameter entirely when there's nothing to advertise,
    // so a present-but-empty list is malformed. Rejecting it keeps "parameter
    // present" unambiguous, matching the stricter check in the TS implementation.
    if out.is_empty() {
        return Err(Error::InvalidProtocol(
            "empty application_protocols".to_string(),
        ));
    }
    Ok(out)
}

/// Decode a single VarInt parameter, validating that the entire payload is consumed.
fn decode_varint_param(data: &mut Bytes) -> Result<u64, Error> {
    let value = VarInt::decode(data)?.into_inner();
    if data.has_remaining() {
        return Err(Error::Short);
    }
    Ok(value)
}

/// Returns the encoded size of a varint value.
fn varint_size(v: u64) -> usize {
    if v < (1 << 6) {
        1
    } else if v < (1 << 14) {
        2
    } else if v < (1 << 30) {
        4
    } else {
        8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_round_trip() {
        let params = TransportParams {
            initial_max_data: 1024,
            protocols: vec!["moq-lite-04".to_string(), "moq-lite-03".to_string()],
            ..TransportParams::default()
        };
        let decoded = TransportParams::decode(params.encode().unwrap()).unwrap();
        assert_eq!(decoded.protocols, params.protocols);
        assert_eq!(decoded.initial_max_data, 1024);
    }

    #[test]
    fn protocols_omitted_when_empty() {
        // No application_protocols param on the wire when the list is empty, so
        // a peer that never negotiates stays byte-identical to the old format.
        let bytes = TransportParams::default().encode().unwrap();
        assert!(!bytes
            .windows(8)
            .any(|w| w == APPLICATION_PROTOCOLS_ID.to_be_bytes()));
        assert!(TransportParams::decode(bytes).unwrap().protocols.is_empty());
    }

    #[test]
    fn duplicate_protocols_param_rejected() {
        let one = TransportParams {
            protocols: vec!["a".to_string()],
            ..TransportParams::default()
        }
        .encode()
        .unwrap();
        let mut doubled = BytesMut::from(&one[..]);
        doubled.extend_from_slice(&one);
        assert!(matches!(
            TransportParams::decode(doubled.freeze()),
            Err(Error::DuplicateParam(APPLICATION_PROTOCOLS_ID))
        ));
    }

    #[test]
    fn invalid_utf8_protocol_rejected() {
        // id=APPLICATION_PROTOCOLS_ID, len=2, value=[len=1, 0xff]
        let mut buf = BytesMut::new();
        APPLICATION_PROTOCOLS_ID_VI.encode(&mut buf);
        VarInt::from_u32(2).encode(&mut buf);
        VarInt::from_u32(1).encode(&mut buf);
        buf.put_u8(0xff);
        assert!(matches!(
            TransportParams::decode(buf.freeze()),
            Err(Error::InvalidProtocol(_))
        ));
    }

    #[test]
    fn path_round_trip() {
        let params = TransportParams {
            initial_max_data: 1024,
            path: Some("/broadcast/room-42".to_string()),
            ..TransportParams::default()
        };
        let decoded = TransportParams::decode(params.encode().unwrap()).unwrap();
        assert_eq!(decoded.path.as_deref(), Some("/broadcast/room-42"));
        assert_eq!(decoded.initial_max_data, 1024);
    }

    // The PATH_ID as it appears on the wire: an 8-byte varint with the top two
    // bits set (0b11), i.e. PATH_ID | 0xc000_0000_0000_0000.
    const PATH_ID_WIRE: [u8; 8] = (PATH_ID | (0b11 << 62)).to_be_bytes();

    #[test]
    fn path_omitted_when_none() {
        // No path param on the wire when unset, so a peer that never addresses a
        // resource stays byte-identical to the old format.
        let bytes = TransportParams::default().encode().unwrap();
        assert!(!bytes.windows(8).any(|w| w == PATH_ID_WIRE));
        assert!(TransportParams::decode(bytes).unwrap().path.is_none());
    }

    #[test]
    fn empty_path_round_trips_as_some() {
        // An explicitly empty path is distinct from an absent one: the parameter
        // is present on the wire and decodes back to `Some("")`.
        let params = TransportParams {
            path: Some(String::new()),
            ..TransportParams::default()
        };
        let bytes = params.encode().unwrap();
        assert!(bytes.windows(8).any(|w| w == PATH_ID_WIRE));
        assert_eq!(
            TransportParams::decode(bytes).unwrap().path.as_deref(),
            Some("")
        );
    }

    #[test]
    fn duplicate_path_param_rejected() {
        let one = TransportParams {
            path: Some("/a".to_string()),
            ..TransportParams::default()
        }
        .encode()
        .unwrap();
        let mut doubled = BytesMut::from(&one[..]);
        doubled.extend_from_slice(&one);
        assert!(matches!(
            TransportParams::decode(doubled.freeze()),
            Err(Error::DuplicateParam(PATH_ID))
        ));
    }

    #[test]
    fn invalid_utf8_path_rejected() {
        // id=PATH_ID, len=1, value=[0xff]
        let mut buf = BytesMut::new();
        PATH_ID_VI.encode(&mut buf);
        VarInt::from_u32(1).encode(&mut buf);
        buf.put_u8(0xff);
        assert!(matches!(
            TransportParams::decode(buf.freeze()),
            Err(Error::InvalidPath(_))
        ));
    }

    #[test]
    fn empty_protocols_param_rejected() {
        // id=APPLICATION_PROTOCOLS_ID, len=0 — never produced by the encoder.
        let mut buf = BytesMut::new();
        APPLICATION_PROTOCOLS_ID_VI.encode(&mut buf);
        VarInt::from_u32(0).encode(&mut buf);
        assert!(matches!(
            TransportParams::decode(buf.freeze()),
            Err(Error::InvalidProtocol(_))
        ));
    }
}
