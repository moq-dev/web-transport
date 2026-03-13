use bytes::Bytes;

use crate::Error;

/// Abstracts message I/O over a reliable transport.
///
/// Each `send`/`recv` operates on a single complete message (frame).
/// For WebSocket, this maps to individual WS binary messages.
/// For TCP/TLS byte streams, the transport handles frame delimiting.
pub trait Transport: Send + 'static {
    /// Send a message.
    fn send(&mut self, data: Bytes) -> impl std::future::Future<Output = Result<(), Error>> + Send;

    /// Receive the next complete message.
    fn recv(&mut self) -> impl std::future::Future<Output = Result<Bytes, Error>> + Send;

    /// Gracefully close the transport.
    fn close(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}

// StreamTransport: message I/O over a byte stream (TCP/TLS).
// Handles QMux frame delimiting to return complete frames as Bytes.
#[cfg(feature = "tcp")]
mod stream_transport {
    use bytes::{BufMut, Bytes, BytesMut};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
    use web_transport_proto::VarInt;

    use super::Transport;
    use crate::{Error, MAX_FRAME_SIZE};

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

        /// Read a varint, appending raw bytes to buf. Returns the decoded value.
        async fn read_varint(&mut self, buf: &mut BytesMut) -> Result<VarInt, Error> {
            let first = self.reader.read_u8().await?;
            buf.put_u8(first);

            let tag = first >> 6;
            let len = 1usize << tag;

            if len == 1 {
                return Ok(VarInt::try_from((first & 0x3f) as u64).unwrap());
            }

            let start = buf.len();
            buf.resize(start + len - 1, 0);
            self.reader.read_exact(&mut buf[start..]).await?;

            let mut raw = [0u8; 8];
            raw[0] = first & 0x3f;
            raw[1..len].copy_from_slice(&buf[start..start + len - 1]);

            let value = match len {
                2 => u16::from_be_bytes([raw[0], raw[1]]) as u64,
                4 => u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
                8 => u64::from_be_bytes(raw),
                _ => unreachable!(),
            };

            VarInt::try_from(value).map_err(|_| Error::Short)
        }

        /// Read exactly `len` bytes, appending to buf.
        async fn read_bytes(&mut self, len: usize, buf: &mut BytesMut) -> Result<(), Error> {
            let start = buf.len();
            buf.resize(start + len, 0);
            self.reader.read_exact(&mut buf[start..]).await?;
            Ok(())
        }

        /// Read one complete QMux frame from the byte stream, returning raw bytes.
        async fn recv_qmux_frame(&mut self) -> Result<Bytes, Error> {
            let mut buf = BytesMut::new();
            let frame_type = self.read_varint(&mut buf).await?.into_inner();

            // STREAM frames: 0x08-0x0f
            if (0x08..=0x0f).contains(&frame_type) {
                let has_off = frame_type & 0x04 != 0;
                let has_len = frame_type & 0x02 != 0;

                self.read_varint(&mut buf).await?; // stream id

                if has_off {
                    self.read_varint(&mut buf).await?; // offset
                }

                if has_len {
                    let len = self.read_varint(&mut buf).await?.into_inner() as usize;
                    if len > MAX_FRAME_SIZE {
                        return Err(Error::FrameTooLarge);
                    }
                    self.read_bytes(len, &mut buf).await?;
                } else {
                    return Err(Error::Short);
                }

                return Ok(buf.freeze());
            }

            match frame_type {
                // RESET_STREAM
                0x04 => {
                    self.read_varint(&mut buf).await?; // id
                    self.read_varint(&mut buf).await?; // code
                    self.read_varint(&mut buf).await?; // final_size
                }
                // STOP_SENDING
                0x05 => {
                    self.read_varint(&mut buf).await?; // id
                    self.read_varint(&mut buf).await?; // code
                }
                // CONNECTION_CLOSE / APPLICATION_CLOSE
                0x1c | 0x1d => {
                    self.read_varint(&mut buf).await?; // code
                    self.read_varint(&mut buf).await?; // frame_type
                    let reason_len = self.read_varint(&mut buf).await?.into_inner() as usize;
                    if reason_len > MAX_FRAME_SIZE {
                        return Err(Error::FrameTooLarge);
                    }
                    self.read_bytes(reason_len, &mut buf).await?;
                }
                // MAX_DATA
                0x10 => {
                    self.read_varint(&mut buf).await?;
                }
                // MAX_STREAM_DATA
                0x11 => {
                    self.read_varint(&mut buf).await?; // id
                    self.read_varint(&mut buf).await?; // max
                }
                // MAX_STREAMS (bidi/uni)
                0x12 | 0x13 => {
                    self.read_varint(&mut buf).await?;
                }
                // DATA_BLOCKED
                0x14 => {
                    self.read_varint(&mut buf).await?;
                }
                // STREAM_DATA_BLOCKED
                0x15 => {
                    self.read_varint(&mut buf).await?; // id
                    self.read_varint(&mut buf).await?; // limit
                }
                // STREAMS_BLOCKED (bidi/uni)
                0x16 | 0x17 => {
                    self.read_varint(&mut buf).await?;
                }
                // DATAGRAM without length — can't delimit on a byte stream
                0x30 => return Err(Error::InvalidFrameType(frame_type)),
                // DATAGRAM with length
                0x31 => {
                    let len = self.read_varint(&mut buf).await?.into_inner() as usize;
                    if len > MAX_FRAME_SIZE {
                        return Err(Error::FrameTooLarge);
                    }
                    self.read_bytes(len, &mut buf).await?;
                }
                // QX_TRANSPORT_PARAMETERS
                0x3f5153300d0a0d0a => {
                    let len = self.read_varint(&mut buf).await?.into_inner() as usize;
                    if len > MAX_FRAME_SIZE {
                        return Err(Error::FrameTooLarge);
                    }
                    self.read_bytes(len, &mut buf).await?;
                }
                _ => return Err(Error::InvalidFrameType(frame_type)),
            }

            Ok(buf.freeze())
        }
    }

    impl<T: AsyncRead + AsyncWrite + Send + 'static> Transport for StreamTransport<T> {
        async fn send(&mut self, data: Bytes) -> Result<(), Error> {
            self.writer.write_all(&data).await?;
            self.writer.flush().await?;
            Ok(())
        }

        async fn recv(&mut self) -> Result<Bytes, Error> {
            self.recv_qmux_frame().await
        }

        async fn close(&mut self) -> Result<(), Error> {
            self.writer.shutdown().await?;
            Ok(())
        }
    }
}

#[cfg(feature = "tcp")]
pub(crate) use stream_transport::StreamTransport;

// WsTransport: message I/O over WebSocket.
#[cfg(feature = "ws")]
mod ws_transport {
    use bytes::Bytes;
    use tokio_tungstenite::tungstenite;

    use super::Transport;
    use crate::Error;

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
        async fn send(&mut self, data: Bytes) -> Result<(), Error> {
            use futures::SinkExt;

            self.ws
                .send(tungstenite::Message::Binary(data))
                .await
                .map_err(|_| Error::Closed)?;
            Ok(())
        }

        async fn recv(&mut self) -> Result<Bytes, Error> {
            use futures::StreamExt;

            loop {
                let message = self.ws.next().await.ok_or(Error::Closed)??;
                match message {
                    tungstenite::Message::Binary(data) => {
                        return Ok(data);
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
