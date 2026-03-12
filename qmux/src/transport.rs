use crate::{Error, Frame, Version};

/// Abstracts frame I/O over different transports.
pub trait Transport: Send + 'static {
    fn send_frame(
        &mut self,
        frame: Frame,
        version: Version,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send;

    fn recv_frame(
        &mut self,
        version: Version,
    ) -> impl std::future::Future<Output = Result<Option<Frame>, Error>> + Send;

    fn close(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}

// StreamTransport: frame I/O over a byte stream (TCP/TLS).
#[cfg(feature = "tcp")]
mod stream_transport {
    use bytes::{Bytes, BytesMut};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
    use web_transport_proto::VarInt;

    use super::Transport;
    use crate::{Error, Frame, Version};

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

        async fn read_varint(&mut self) -> Result<VarInt, Error> {
            let first = self.reader.read_u8().await?;
            let tag = first >> 6;
            let len = 1usize << tag;

            if len == 1 {
                return Ok(VarInt::try_from((first & 0x3f) as u64).unwrap());
            }

            let mut buf = [0u8; 8];
            buf[0] = first & 0x3f;
            self.reader.read_exact(&mut buf[1..len]).await?;

            let value = match len {
                2 => u16::from_be_bytes([buf[0], buf[1]]) as u64,
                4 => u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64,
                8 => u64::from_be_bytes(buf),
                _ => unreachable!(),
            };

            VarInt::try_from(value).map_err(|_| Error::Short)
        }

        async fn read_bytes(&mut self, len: usize) -> Result<Bytes, Error> {
            let mut buf = BytesMut::zeroed(len);
            self.reader.read_exact(&mut buf).await?;
            Ok(buf.freeze())
        }
    }

    impl<T: AsyncRead + AsyncWrite + Send + 'static> Transport for StreamTransport<T> {
        async fn send_frame(&mut self, frame: Frame, version: Version) -> Result<(), Error> {
            let data = frame.encode(version);
            self.writer.write_all(&data).await?;
            self.writer.flush().await?;
            Ok(())
        }

        async fn recv_frame(&mut self, version: Version) -> Result<Option<Frame>, Error> {
            match version {
                Version::WebTransport => {
                    Err(Error::Io("WebTransport version not supported over byte streams".into()))
                }
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
                    return Err(Error::Short);
                };

                return Ok(Some(Frame::Stream(crate::proto::Stream {
                    id,
                    data,
                    fin: has_fin,
                })));
            }

            match frame_type {
                // RESET_STREAM
                0x04 => {
                    let id = crate::StreamId(self.read_varint().await?);
                    let code = self.read_varint().await?;
                    let _final_size = self.read_varint().await?;
                    Ok(Some(Frame::ResetStream(crate::proto::ResetStream { id, code })))
                }
                // STOP_SENDING
                0x05 => {
                    let id = crate::StreamId(self.read_varint().await?);
                    let code = self.read_varint().await?;
                    Ok(Some(Frame::StopSending(crate::proto::StopSending { id, code })))
                }
                // CONNECTION_CLOSE / APPLICATION_CLOSE
                0x1c | 0x1d => {
                    let code = self.read_varint().await?;
                    let _frame_type = self.read_varint().await?;
                    let reason_len = self.read_varint().await?.into_inner() as usize;
                    let reason_bytes = self.read_bytes(reason_len).await?;
                    let reason = String::from_utf8_lossy(&reason_bytes).into_owned();
                    Ok(Some(Frame::ConnectionClose(crate::proto::ConnectionClose {
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
                // DATAGRAM without length — cannot determine payload boundary on a stream
                0x30 => Err(Error::InvalidFrameType(frame_type)),
                // DATAGRAM with length
                0x31 => {
                    let len = self.read_varint().await?.into_inner() as usize;
                    let _data = self.read_bytes(len).await?;
                    Ok(None)
                }
                // QX_TRANSPORT_PARAMETERS
                0x3f5153300d0a0d0a => {
                    let len = self.read_varint().await?.into_inner() as usize;
                    let _data = self.read_bytes(len).await?;
                    Ok(None)
                }
                // Unknown frame types on a stream — can't skip without knowing length
                _ => Err(Error::InvalidFrameType(frame_type)),
            }
        }
    }
}

#[cfg(feature = "tcp")]
pub(crate) use stream_transport::StreamTransport;

// WsTransport: frame I/O over WebSocket.
#[cfg(feature = "ws")]
mod ws_transport {
    use tokio_tungstenite::tungstenite;

    use super::Transport;
    use crate::{Error, Frame, Version};

    pub(crate) struct WsTransport<T> {
        ws: T,
    }

    impl<T> WsTransport<T>
    where
        T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
            + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        pub fn new(ws: T) -> Self {
            Self { ws }
        }
    }

    impl<T> Transport for WsTransport<T>
    where
        T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
            + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        async fn send_frame(&mut self, frame: Frame, version: Version) -> Result<(), Error> {
            use futures::SinkExt;

            let data = frame.encode(version);
            self.ws
                .send(tungstenite::Message::Binary(data.to_vec()))
                .await
                .map_err(|_| Error::Closed)?;
            Ok(())
        }

        async fn recv_frame(&mut self, version: Version) -> Result<Option<Frame>, Error> {
            use futures::StreamExt;

            loop {
                let message = self.ws.next().await.ok_or(Error::Closed)??;
                match message {
                    tungstenite::Message::Binary(data) => {
                        return Frame::decode(data.into(), version);
                    }
                    tungstenite::Message::Close(_) => {
                        return Err(Error::Closed);
                    }
                    tungstenite::Message::Ping(data) => {
                        use futures::SinkExt;
                        self.ws
                            .send(tungstenite::Message::Pong(data))
                            .await
                            .map_err(|_| Error::Closed)?;
                        continue;
                    }
                    tungstenite::Message::Text(_)
                    | tungstenite::Message::Pong(_)
                    | tungstenite::Message::Frame(_) => {
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
}

#[cfg(feature = "ws")]
pub(crate) use ws_transport::WsTransport;
