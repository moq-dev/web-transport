use bytes::{Buf, Bytes, BytesMut};
use web_transport_proto::VarInt;

use crate::Error;

/// Transport parameters exchanged during QMux connection setup.
///
/// These mirror the QUIC transport parameters from RFC 9000, Section 18.2.
/// All values default to 0 (per QUIC), meaning no data/streams allowed
/// until the peer advertises its limits.
#[derive(Debug, Clone, Default)]
pub struct TransportParams {
    pub initial_max_data: u64,                   // ID 0x04
    pub initial_max_stream_data_bidi_local: u64, // ID 0x05
    pub initial_max_stream_data_bidi_remote: u64, // ID 0x06
    pub initial_max_stream_data_uni: u64,        // ID 0x07
    pub initial_max_streams_bidi: u64,           // ID 0x08
    pub initial_max_streams_uni: u64,            // ID 0x09
}

impl TransportParams {
    /// Recommended defaults for a receiver that wants reasonable flow control.
    pub fn recommended() -> Self {
        Self {
            initial_max_data: 1_048_576,                    // 1 MB
            initial_max_stream_data_bidi_local: 262_144,    // 256 KB
            initial_max_stream_data_bidi_remote: 262_144,   // 256 KB
            initial_max_stream_data_uni: 262_144,           // 256 KB
            initial_max_streams_bidi: 100,
            initial_max_streams_uni: 100,
        }
    }

    // Transport parameter IDs (all fit in u8)
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

        write_param(&mut buf, Self::INITIAL_MAX_DATA, self.initial_max_data)?;
        write_param(&mut buf, Self::INITIAL_MAX_STREAM_DATA_BIDI_LOCAL, self.initial_max_stream_data_bidi_local)?;
        write_param(&mut buf, Self::INITIAL_MAX_STREAM_DATA_BIDI_REMOTE, self.initial_max_stream_data_bidi_remote)?;
        write_param(&mut buf, Self::INITIAL_MAX_STREAM_DATA_UNI, self.initial_max_stream_data_uni)?;
        write_param(&mut buf, Self::INITIAL_MAX_STREAMS_BIDI, self.initial_max_streams_bidi)?;
        write_param(&mut buf, Self::INITIAL_MAX_STREAMS_UNI, self.initial_max_streams_uni)?;

        Ok(buf.freeze())
    }

    /// Decode transport parameters from bytes.
    pub fn decode(mut data: Bytes) -> Result<Self, Error> {
        let mut params = TransportParams::default();

        while data.has_remaining() {
            let id = VarInt::decode(&mut data)?.into_inner();
            let len = VarInt::decode(&mut data)?.into_inner() as usize;

            if data.remaining() < len {
                return Err(Error::Short);
            }

            let mut param_data = data.split_to(len);

            match id {
                0x04 => params.initial_max_data = VarInt::decode(&mut param_data)?.into_inner(),
                0x05 => {
                    params.initial_max_stream_data_bidi_local =
                        VarInt::decode(&mut param_data)?.into_inner()
                }
                0x06 => {
                    params.initial_max_stream_data_bidi_remote =
                        VarInt::decode(&mut param_data)?.into_inner()
                }
                0x07 => {
                    params.initial_max_stream_data_uni =
                        VarInt::decode(&mut param_data)?.into_inner()
                }
                0x08 => {
                    params.initial_max_streams_bidi =
                        VarInt::decode(&mut param_data)?.into_inner()
                }
                0x09 => {
                    params.initial_max_streams_uni =
                        VarInt::decode(&mut param_data)?.into_inner()
                }
                _ => {
                    // Unknown parameter, skip (already split off)
                }
            }
        }

        Ok(params)
    }
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
