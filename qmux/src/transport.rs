use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use web_transport_proto::VarInt;

use crate::{Error, Frame, Version};

/// Abstracts frame I/O over different transports.
pub(crate) trait FrameTransport: Send + 'static {
    fn send_frame(
        &mut self,
        frame: Frame,
        version: Version,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send;

    fn recv_frame(
        &mut self,
        version: Version,
    ) -> impl std::future::Future<Output = Result<Option<Frame>, Error>> + Send;

    #[allow(dead_code)]
    fn close(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}

/// Frame transport over a byte stream (TCP/TLS).
pub(crate) struct StreamTransport<T> {
    reader: BufReader<tokio::io::ReadHalf<T>>,
    writer: BufWriter<tokio::io::WriteHalf<T>>,
}

impl<T: AsyncRead + AsyncWrite + Send + 'static> StreamTransport<T> {
    pub fn new(stream: T) -> Self {
        let (read, write) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(read),
            writer: BufWriter::new(write),
        }
    }

    /// Read a VarInt from the stream.
    async fn read_varint(&mut self) -> Result<VarInt, Error> {
        let first = self.reader.read_u8().await?;
        let tag = first >> 6;
        let len = 1usize << tag;

        if len == 1 {
            return Ok(VarInt::try_from((first & 0x3f) as u64).unwrap());
        }

        let mut buf = [0u8; 8];
        buf[0] = first & 0x3f; // strip the 2-bit tag
        self.reader.read_exact(&mut buf[1..len]).await?;

        let value = match len {
            2 => u16::from_be_bytes([buf[0], buf[1]]) as u64,
            4 => u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64,
            8 => u64::from_be_bytes(buf),
            _ => unreachable!(),
        };

        Ok(VarInt::try_from(value).unwrap())
    }

    /// Read exact bytes from the stream.
    async fn read_bytes(&mut self, len: usize) -> Result<Bytes, Error> {
        let mut buf = BytesMut::zeroed(len);
        self.reader.read_exact(&mut buf).await?;
        Ok(buf.freeze())
    }
}

impl<T: AsyncRead + AsyncWrite + Send + 'static> FrameTransport for StreamTransport<T> {
    async fn send_frame(&mut self, frame: Frame, version: Version) -> Result<(), Error> {
        let data = frame.encode(version);
        self.writer.write_all(&data).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn recv_frame(&mut self, version: Version) -> Result<Option<Frame>, Error> {
        match version {
            Version::WebTransport => unreachable!("WebTransport version not used over streams"),
            Version::QMux00 => self.recv_frame_qmux().await,
        }
    }

    async fn close(&mut self) -> Result<(), Error> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

impl<T: AsyncRead + AsyncWrite + Send + 'static> StreamTransport<T> {
    async fn recv_frame_qmux(&mut self) -> Result<Option<Frame>, Error> {
        let frame_type = self.read_varint().await?.into_inner();

        // STREAM frames: 0x08-0x0f
        if (0x08..=0x0f).contains(&frame_type) {
            let has_off = frame_type & 0x04 != 0;
            let has_len = frame_type & 0x02 != 0;
            let has_fin = frame_type & 0x01 != 0;

            let id = crate::StreamId(self.read_varint().await?);

            if has_off {
                let _offset = self.read_varint().await?;
            }

            let data = if has_len {
                let len = self.read_varint().await?.into_inner() as usize;
                self.read_bytes(len).await?
            } else {
                // Without LEN, we can't know where frame ends on a stream.
                // This shouldn't happen in practice — we always encode with LEN.
                return Err(Error::Short);
            };

            return Ok(Some(Frame::Stream(crate::Stream {
                id,
                data,
                fin: has_fin,
            })));
        }

        match frame_type {
            0x04 => {
                // RESET_STREAM
                let id = crate::StreamId(self.read_varint().await?);
                let code = self.read_varint().await?;
                let _final_size = self.read_varint().await?;
                Ok(Some(Frame::ResetStream(crate::ResetStream { id, code })))
            }
            0x05 => {
                // STOP_SENDING
                let id = crate::StreamId(self.read_varint().await?);
                let code = self.read_varint().await?;
                Ok(Some(Frame::StopSending(crate::StopSending { id, code })))
            }
            0x1c | 0x1d => {
                // CONNECTION_CLOSE / APPLICATION_CLOSE
                let code = self.read_varint().await?;
                let _frame_type = self.read_varint().await?;
                let reason_len = self.read_varint().await?.into_inner() as usize;
                let reason_bytes = self.read_bytes(reason_len).await?;
                let reason = String::from_utf8_lossy(&reason_bytes).into_owned();
                Ok(Some(Frame::ConnectionClose(crate::ConnectionClose {
                    code,
                    reason,
                })))
            }
            // Flow control — decode fields, drop
            0x10 => {
                let _max = self.read_varint().await?;
                Ok(None)
            }
            0x11 => {
                let _id = self.read_varint().await?;
                let _max = self.read_varint().await?;
                Ok(None)
            }
            0x12 | 0x13 => {
                let _max = self.read_varint().await?;
                Ok(None)
            }
            0x14 => {
                let _limit = self.read_varint().await?;
                Ok(None)
            }
            0x15 => {
                let _id = self.read_varint().await?;
                let _limit = self.read_varint().await?;
                Ok(None)
            }
            0x16 | 0x17 => {
                let _limit = self.read_varint().await?;
                Ok(None)
            }
            // DATAGRAM with length
            0x31 => {
                let len = self.read_varint().await?.into_inner() as usize;
                let _data = self.read_bytes(len).await?;
                Ok(None)
            }
            // QX_TRANSPORT_PARAMETERS — has its own framing with length
            0x3f5153300d0a0d0a => {
                // Read the parameters length and skip
                let len = self.read_varint().await?.into_inner() as usize;
                let _data = self.read_bytes(len).await?;
                Ok(None)
            }
            // Unknown frame types — we can't skip them without knowing their length on a stream
            // For safety, treat as error
            _ => Err(Error::InvalidFrameType(frame_type)),
        }
    }
}

/// Frame transport over WebSocket.
#[cfg(feature = "websocket")]
pub(crate) struct WsTransport<T> {
    ws: T,
}

#[cfg(feature = "websocket")]
impl<T> WsTransport<T>
where
    T: futures::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + futures::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    pub fn new(ws: T) -> Self {
        Self { ws }
    }
}

#[cfg(feature = "websocket")]
impl<T> FrameTransport for WsTransport<T>
where
    T: futures::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + futures::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    async fn send_frame(&mut self, frame: Frame, version: Version) -> Result<(), Error> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        let data = frame.encode(version);
        self.ws
            .send(Message::Binary(data.to_vec()))
            .await
            .map_err(|_| Error::Closed)?;
        Ok(())
    }

    async fn recv_frame(&mut self, version: Version) -> Result<Option<Frame>, Error> {
        use futures::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        loop {
            let message = self.ws.next().await.ok_or(Error::Closed)??;
            match message {
                Message::Binary(data) => {
                    return Frame::decode(data.into(), version);
                }
                Message::Close(_) => {
                    return Err(Error::Closed);
                }
                Message::Ping(data) => {
                    use futures::SinkExt;
                    self.ws
                        .send(Message::Pong(data))
                        .await
                        .map_err(|_| Error::Closed)?;
                    continue;
                }
                Message::Text(_) | Message::Pong(_) | Message::Frame(_) => {
                    continue;
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Error> {
        use futures::SinkExt;
        self.ws.close().await.map_err(|_| Error::Closed)?;
        Ok(())
    }
}
