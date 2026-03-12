use bytes::{Buf, BufMut, Bytes, BytesMut};
use web_transport_proto::VarInt;

use crate::{Error, StreamId, Version};

#[derive(Debug, Clone)]
pub struct Stream {
    pub id: StreamId,
    pub data: Bytes,
    pub fin: bool,
}

#[derive(Debug, Clone)]
pub struct ResetStream {
    pub id: StreamId,
    pub code: VarInt,
}

#[derive(Debug, Clone)]
pub struct StopSending {
    pub id: StreamId,
    pub code: VarInt,
}

#[derive(Debug, Clone)]
pub struct ConnectionClose {
    pub code: VarInt,
    pub reason: String,
}

/// QUIC-compatible frames for multiplexed transport
#[derive(Debug)]
pub enum Frame {
    ResetStream(ResetStream),
    StopSending(StopSending),
    ConnectionClose(ConnectionClose),
    Stream(Stream),
}

impl Frame {
    pub fn encode(&self, version: Version) -> Bytes {
        let mut buf = BytesMut::new();

        match version {
            Version::WebTransport => self.encode_wt(&mut buf),
            Version::QMux00 => self.encode_qmux(&mut buf),
        }

        buf.freeze()
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
        }
    }

    fn encode_qmux(&self, buf: &mut BytesMut) {
        match self {
            Frame::Stream(s) => {
                // Always LEN bit (0x02), never OFF bit. Type = 0x0a | fin_bit
                let frame_type = 0x08u64 | 0x02 | if s.fin { 0x01 } else { 0 };
                VarInt::try_from(frame_type).unwrap().encode(buf);
                s.id.0.encode(buf);
                VarInt::try_from(s.data.len() as u64).unwrap().encode(buf);
                buf.put_slice(&s.data);
            }
            Frame::ResetStream(r) => {
                VarInt::try_from(0x04u64).unwrap().encode(buf);
                r.id.0.encode(buf);
                r.code.encode(buf);
                // final_size = 0 (no flow control tracking)
                VarInt::from(0u32).encode(buf);
            }
            Frame::StopSending(s) => {
                VarInt::try_from(0x05u64).unwrap().encode(buf);
                s.id.0.encode(buf);
                s.code.encode(buf);
            }
            Frame::ConnectionClose(c) => {
                // APPLICATION_CLOSE (0x1d)
                VarInt::try_from(0x1du64).unwrap().encode(buf);
                c.code.encode(buf);
                // frame_type = 0 (application close)
                VarInt::from(0u32).encode(buf);
                let reason_bytes = c.reason.as_bytes();
                VarInt::try_from(reason_bytes.len() as u64).unwrap().encode(buf);
                buf.put_slice(reason_bytes);
            }
        }
    }

    #[allow(dead_code)]
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
                Ok(Frame::Stream(Stream { id, data, fin: false }))
            }
            0x09 => {
                let id = StreamId(VarInt::decode(&mut data)?);
                Ok(Frame::Stream(Stream { id, data, fin: true }))
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
                let len = VarInt::decode(&mut data)?.into_inner() as usize;
                if data.remaining() < len {
                    return Err(Error::Short);
                }
                data.split_to(len)
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
                let reason_len = VarInt::decode(&mut data)?.into_inner() as usize;
                if data.remaining() < reason_len {
                    return Err(Error::Short);
                }
                let reason = String::from_utf8_lossy(&data.split_to(reason_len)).into_owned();
                Ok(Some(Frame::ConnectionClose(ConnectionClose { code, reason })))
            }
            // MAX_DATA
            0x10 => {
                let _max = VarInt::decode(&mut data)?;
                Ok(None)
            }
            // MAX_STREAM_DATA
            0x11 => {
                let _id = VarInt::decode(&mut data)?;
                let _max = VarInt::decode(&mut data)?;
                Ok(None)
            }
            // MAX_STREAMS (bidi/uni)
            0x12 | 0x13 => {
                let _max = VarInt::decode(&mut data)?;
                Ok(None)
            }
            // DATA_BLOCKED
            0x14 => {
                let _limit = VarInt::decode(&mut data)?;
                Ok(None)
            }
            // STREAM_DATA_BLOCKED
            0x15 => {
                let _id = VarInt::decode(&mut data)?;
                let _limit = VarInt::decode(&mut data)?;
                Ok(None)
            }
            // STREAMS_BLOCKED (bidi/uni)
            0x16 | 0x17 => {
                let _limit = VarInt::decode(&mut data)?;
                Ok(None)
            }
            // DATAGRAM / DATAGRAM with length
            0x30 | 0x31 => Ok(None),
            // QX_TRANSPORT_PARAMETERS
            0x3f5153300d0a0d0a => Ok(None),
            // Unknown — ignore
            _ => Ok(None),
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
