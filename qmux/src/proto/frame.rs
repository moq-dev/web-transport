use bytes::{Buf, BufMut, Bytes, BytesMut};
use web_transport_proto::VarInt;

use crate::{Error, StreamId, TransportParams, Version};

// QMux frame type IDs (QUIC v1 compatible)
const RESET_STREAM: VarInt = VarInt::from_u32(0x04);
const STOP_SENDING: VarInt = VarInt::from_u32(0x05);
const STREAM_BASE: u32 = 0x08;
const MAX_DATA: VarInt = VarInt::from_u32(0x10);
const MAX_STREAM_DATA: VarInt = VarInt::from_u32(0x11);
const MAX_STREAMS_BIDI: VarInt = VarInt::from_u32(0x12);
const MAX_STREAMS_UNI: VarInt = VarInt::from_u32(0x13);
const DATA_BLOCKED: VarInt = VarInt::from_u32(0x14);
const STREAM_DATA_BLOCKED: VarInt = VarInt::from_u32(0x15);
const STREAMS_BLOCKED_BIDI: VarInt = VarInt::from_u32(0x16);
const STREAMS_BLOCKED_UNI: VarInt = VarInt::from_u32(0x17);
const APPLICATION_CLOSE: VarInt = VarInt::from_u32(0x1d);

// QX_TRANSPORT_PARAMETERS magic: "\xffQMX\r\n\r\n"
// This exceeds u32 range, so we use try_from at decode time and a pre-computed const for encode.
const QX_TRANSPORT_PARAMETERS: u64 = 0x3f5153300d0a0d0a;
const QX_TRANSPORT_PARAMETERS_VI: VarInt = unsafe { VarInt::from_u64_unchecked(QX_TRANSPORT_PARAMETERS) };

/// Stream data frame carrying payload bytes for a specific stream.
#[derive(Debug, Clone)]
pub struct Stream {
    /// The stream this data belongs to.
    pub id: StreamId,
    /// The payload bytes.
    pub data: Bytes,
    /// Whether this is the final frame on the stream.
    pub fin: bool,
}

/// Abruptly terminates the sending side of a stream with an error code.
#[derive(Debug, Clone)]
pub struct ResetStream {
    /// The stream being reset.
    pub id: StreamId,
    /// Application-defined error code.
    pub code: VarInt,
}

/// Requests that the peer stop sending on a stream.
#[derive(Debug, Clone)]
pub struct StopSending {
    /// The stream to stop.
    pub id: StreamId,
    /// Application-defined error code.
    pub code: VarInt,
}

/// Closes the entire connection with an error code and reason.
#[derive(Debug, Clone)]
pub struct ConnectionClose {
    /// Application-defined error code.
    pub code: VarInt,
    /// Human-readable reason for closing.
    pub reason: String,
}

/// A QUIC-compatible frame for multiplexed transport.
#[derive(Debug, Clone)]
pub enum Frame {
    ResetStream(ResetStream),
    StopSending(StopSending),
    ConnectionClose(ConnectionClose),
    Stream(Stream),
    MaxData(u64),
    MaxStreamData { id: StreamId, max: u64 },
    MaxStreamsBidi(u64),
    MaxStreamsUni(u64),
    DataBlocked(u64),
    StreamDataBlocked { id: StreamId, limit: u64 },
    StreamsBlockedBidi(u64),
    StreamsBlockedUni(u64),
    TransportParameters(TransportParams),
}

impl Frame {
    /// Encode the frame into bytes using the given wire format version.
    pub fn encode(&self, version: Version) -> Result<Bytes, Error> {
        let mut buf = BytesMut::new();

        match version {
            Version::WebTransport => self.encode_wt(&mut buf),
            Version::QMux00 => self.encode_qmux(&mut buf)?,
        }

        Ok(buf.freeze())
    }

    fn encode_wt(&self, buf: &mut BytesMut) {
        match self {
            Frame::Stream(s) => {
                buf.put_u8(if s.fin { 0x09 } else { 0x08 });
                s.id.0.encode(buf);
                buf.put_slice(&s.data);
            }
            Frame::ResetStream(r) => {
                buf.put_u8(0x04);
                r.id.0.encode(buf);
                r.code.encode(buf);
            }
            Frame::StopSending(s) => {
                buf.put_u8(0x05);
                s.id.0.encode(buf);
                s.code.encode(buf);
            }
            Frame::ConnectionClose(c) => {
                buf.put_u8(0x1d);
                c.code.encode(buf);
                buf.put_slice(c.reason.as_bytes());
            }
            // Flow control frames are QMux-only
            _ => unreachable!("flow control frames are not supported in WebTransport version"),
        }
    }

    fn encode_qmux(&self, buf: &mut BytesMut) -> Result<(), Error> {
        match self {
            Frame::Stream(s) => {
                // Always LEN bit (0x02), never OFF bit. Type = 0x0a | fin_bit
                let frame_type = VarInt::from_u32(STREAM_BASE | 0x02 | if s.fin { 0x01 } else { 0 });
                frame_type.encode(buf);
                s.id.0.encode(buf);
                VarInt::try_from(s.data.len())?.encode(buf);
                buf.put_slice(&s.data);
            }
            Frame::ResetStream(r) => {
                RESET_STREAM.encode(buf);
                r.id.0.encode(buf);
                r.code.encode(buf);
                // final_size = 0 (no flow control tracking yet)
                VarInt::from(0u32).encode(buf);
            }
            Frame::StopSending(s) => {
                STOP_SENDING.encode(buf);
                s.id.0.encode(buf);
                s.code.encode(buf);
            }
            Frame::ConnectionClose(c) => {
                APPLICATION_CLOSE.encode(buf);
                c.code.encode(buf);
                // frame_type = 0 (application close)
                VarInt::from(0u32).encode(buf);
                let reason_bytes = c.reason.as_bytes();
                VarInt::try_from(reason_bytes.len())?.encode(buf);
                buf.put_slice(reason_bytes);
            }
            Frame::MaxData(max) => {
                MAX_DATA.encode(buf);
                VarInt::try_from(*max)?.encode(buf);
            }
            Frame::MaxStreamData { id, max } => {
                MAX_STREAM_DATA.encode(buf);
                id.0.encode(buf);
                VarInt::try_from(*max)?.encode(buf);
            }
            Frame::MaxStreamsBidi(max) => {
                MAX_STREAMS_BIDI.encode(buf);
                VarInt::try_from(*max)?.encode(buf);
            }
            Frame::MaxStreamsUni(max) => {
                MAX_STREAMS_UNI.encode(buf);
                VarInt::try_from(*max)?.encode(buf);
            }
            Frame::DataBlocked(limit) => {
                DATA_BLOCKED.encode(buf);
                VarInt::try_from(*limit)?.encode(buf);
            }
            Frame::StreamDataBlocked { id, limit } => {
                STREAM_DATA_BLOCKED.encode(buf);
                id.0.encode(buf);
                VarInt::try_from(*limit)?.encode(buf);
            }
            Frame::StreamsBlockedBidi(limit) => {
                STREAMS_BLOCKED_BIDI.encode(buf);
                VarInt::try_from(*limit)?.encode(buf);
            }
            Frame::StreamsBlockedUni(limit) => {
                STREAMS_BLOCKED_UNI.encode(buf);
                VarInt::try_from(*limit)?.encode(buf);
            }
            Frame::TransportParameters(params) => {
                QX_TRANSPORT_PARAMETERS_VI.encode(buf);
                let payload = params.encode()?;
                VarInt::try_from(payload.len())?.encode(buf);
                buf.put_slice(&payload);
            }
        }

        Ok(())
    }

    /// Decode a frame from bytes using the given wire format version.
    ///
    /// Returns `Ok(None)` for recognized but ignored frame types (e.g. flow control).
    pub fn decode(data: Bytes, version: Version) -> Result<Option<Self>, Error> {
        if data.is_empty() {
            return Err(Error::Short);
        }

        match version {
            Version::WebTransport => Self::decode_wt(data).map(Some),
            Version::QMux00 => Self::decode_qmux(data),
        }
    }

    fn decode_wt(mut data: Bytes) -> Result<Self, Error> {
        let frame_type = data.get_u8();

        match frame_type {
            0x04 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                let code = VarInt::decode(&mut data)?;
                Ok(Frame::ResetStream(ResetStream { id, code }))
            }
            0x05 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                let code = VarInt::decode(&mut data)?;
                Ok(Frame::StopSending(StopSending { id, code }))
            }
            0x08 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                Ok(Frame::Stream(Stream {
                    id,
                    data,
                    fin: false,
                }))
            }
            0x09 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                Ok(Frame::Stream(Stream {
                    id,
                    data,
                    fin: true,
                }))
            }
            0x1d => {
                let code = VarInt::decode(&mut data)?;
                let reason = String::from_utf8_lossy(&data).into_owned();
                Ok(Frame::ConnectionClose(ConnectionClose { code, reason }))
            }
            _ => Err(Error::InvalidFrameType(frame_type as u64)),
        }
    }

    fn decode_qmux(mut data: Bytes) -> Result<Option<Self>, Error> {
        let frame_type = VarInt::decode(&mut data)?.into_inner();

        // STREAM frames: 0x08-0x0f
        if (0x08..=0x0f).contains(&frame_type) {
            let has_off = frame_type & 0x04 != 0;
            let has_len = frame_type & 0x02 != 0;
            let has_fin = frame_type & 0x01 != 0;

            let id = StreamId(VarInt::decode(&mut data)?);

            if has_off {
                let _offset = VarInt::decode(&mut data)?;
            }

            let stream_data = if has_len {
                let len = VarInt::decode(&mut data)?.into_inner();
                if (data.remaining() as u64) < len {
                    return Err(Error::Short);
                }
                data.split_to(len as usize)
            } else {
                data.split_to(data.remaining())
            };

            return Ok(Some(Frame::Stream(Stream {
                id,
                data: stream_data,
                fin: has_fin,
            })));
        }

        match frame_type {
            // RESET_STREAM
            0x04 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                let code = VarInt::decode(&mut data)?;
                let _final_size = VarInt::decode(&mut data)?;
                Ok(Some(Frame::ResetStream(ResetStream { id, code })))
            }
            // STOP_SENDING
            0x05 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                let code = VarInt::decode(&mut data)?;
                Ok(Some(Frame::StopSending(StopSending { id, code })))
            }
            // CONNECTION_CLOSE / APPLICATION_CLOSE
            0x1c | 0x1d => {
                let code = VarInt::decode(&mut data)?;
                let _frame_type = VarInt::decode(&mut data)?;
                let reason_len = VarInt::decode(&mut data)?.into_inner();
                if (data.remaining() as u64) < reason_len {
                    return Err(Error::Short);
                }
                let reason =
                    String::from_utf8_lossy(&data.split_to(reason_len as usize)).into_owned();
                Ok(Some(Frame::ConnectionClose(ConnectionClose {
                    code,
                    reason,
                })))
            }
            // MAX_DATA
            0x10 => {
                let max = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::MaxData(max)))
            }
            // MAX_STREAM_DATA
            0x11 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                let max = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::MaxStreamData { id, max }))
            }
            // MAX_STREAMS (bidi)
            0x12 => {
                let max = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::MaxStreamsBidi(max)))
            }
            // MAX_STREAMS (uni)
            0x13 => {
                let max = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::MaxStreamsUni(max)))
            }
            // DATA_BLOCKED
            0x14 => {
                let limit = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::DataBlocked(limit)))
            }
            // STREAM_DATA_BLOCKED
            0x15 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                let limit = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::StreamDataBlocked { id, limit }))
            }
            // STREAMS_BLOCKED (bidi)
            0x16 => {
                let limit = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::StreamsBlockedBidi(limit)))
            }
            // STREAMS_BLOCKED (uni)
            0x17 => {
                let limit = VarInt::decode(&mut data)?.into_inner();
                Ok(Some(Frame::StreamsBlockedUni(limit)))
            }
            // DATAGRAM without length — rest of message is payload
            0x30 => {
                let _payload = data.split_to(data.remaining());
                Ok(None)
            }
            // DATAGRAM with length
            0x31 => {
                let len = VarInt::decode(&mut data)?.into_inner();
                if (data.remaining() as u64) < len {
                    return Err(Error::Short);
                }
                let _payload = data.split_to(len as usize);
                Ok(None)
            }
            // QX_TRANSPORT_PARAMETERS
            0x3f5153300d0a0d0a => {
                let len = VarInt::decode(&mut data)?.into_inner();
                if (data.remaining() as u64) < len {
                    return Err(Error::Short);
                }
                let payload = data.split_to(len as usize);
                let params = TransportParams::decode(payload)?;
                Ok(Some(Frame::TransportParameters(params)))
            }
            _ => Err(Error::InvalidFrameType(frame_type)),
        }
    }
}

impl From<Stream> for Frame {
    fn from(stream: Stream) -> Self {
        Frame::Stream(stream)
    }
}

impl From<ResetStream> for Frame {
    fn from(reset: ResetStream) -> Self {
        Frame::ResetStream(reset)
    }
}

impl From<StopSending> for Frame {
    fn from(stop: StopSending) -> Self {
        Frame::StopSending(stop)
    }
}

impl From<ConnectionClose> for Frame {
    fn from(close: ConnectionClose) -> Self {
        Frame::ConnectionClose(close)
    }
}
