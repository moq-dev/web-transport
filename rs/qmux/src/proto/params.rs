use bytes::{Buf, Bytes, BytesMut};
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

        Ok(buf.freeze())
    }

    /// Decode transport parameters from bytes.
    pub fn decode(mut data: Bytes) -> Result<Self, Error> {
        let mut params = TransportParams::default();
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
