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

// Stream: message I/O over a byte stream (TCP/TLS/Unix).
// Handles QMux frame delimiting to return complete frames as Bytes.
//
// Cancel safety: a dedicated reader task owns the read half and pushes complete
// frames into an `mpsc` channel. `recv()` is just `rx.recv().await`, which is
// cancel safe — if the future is dropped (e.g. a sibling `tokio::select!` branch
// wins), the buffered frame stays in the channel for the next call. The reader
// task itself never gets cancelled mid-parse, so the multi-step async reads in
// `recv_record`/`recv_qmux00_frame` are safe to keep as-is.
#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
mod stream_transport {
    use bytes::{BufMut, Bytes, BytesMut};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use web_transport_proto::VarInt;

    use super::Transport;
    use crate::{Error, Version, MAX_FRAME_PAYLOAD, MAX_FRAME_SIZE};

    /// Bound on queued frames waiting for the session to drain them. Bytes the
    /// session hasn't picked up yet are also buffered in the OS receive window
    /// once this fills; the channel just gives a small slack so each `recv()`
    /// is a cheap hand-off rather than a syscall.
    const RECV_CHANNEL_CAPACITY: usize = 16;

    /// QMux message I/O over any reliable byte stream (`AsyncRead + AsyncWrite`).
    ///
    /// Handles QMux frame/record delimiting so [`Session`](crate::Session) sees
    /// complete frames. Pair it with [`Session::connect`](crate::Session::connect)
    /// or [`Session::accept`](crate::Session::accept) to run QMux over a transport
    /// the built-in `tcp`/`tls`/`ws` helpers don't cover — a Unix socket, a pipe,
    /// an in-memory duplex, a custom tunnel, etc.:
    ///
    /// ```no_run
    /// # async fn f(stream: tokio::net::TcpStream) -> Result<(), qmux::Error> {
    /// use qmux::transport::Stream;
    /// use qmux::{Config, Session, Version};
    ///
    /// let config = Config::new(Version::QMux01);
    /// let transport = Stream::new(stream, config.version, config.max_record_size);
    /// let session = Session::connect(transport, config);
    /// # let _ = session; Ok(())
    /// # }
    /// ```
    pub struct Stream<T> {
        rx: mpsc::Receiver<Result<Bytes, Error>>,
        writer: BufWriter<tokio::io::WriteHalf<T>>,
        version: Version,
        /// Aborted on drop so the reader task can't outlive the transport.
        reader_task: JoinHandle<()>,
    }

    impl<T: AsyncRead + AsyncWrite + Send + 'static> Stream<T> {
        /// Wrap a byte stream speaking QMux `version`.
        ///
        /// `our_max_record_size` bounds incoming draft-01 records (use
        /// [`Config::max_record_size`](crate::Config::max_record_size)); it is
        /// ignored for draft-00 and the legacy `webtransport` wire format.
        pub fn new(stream: T, version: Version, our_max_record_size: u64) -> Self {
            let (read, write) = tokio::io::split(stream);
            let (tx, rx) = mpsc::channel(RECV_CHANNEL_CAPACITY);
            let reader_task = tokio::spawn(reader_loop(
                BufReader::new(read),
                version,
                our_max_record_size as usize,
                tx,
            ));
            Self {
                rx,
                writer: BufWriter::new(write),
                version,
                reader_task,
            }
        }
    }

    impl<T> Drop for Stream<T> {
        fn drop(&mut self) {
            // Make sure the reader task doesn't outlive the transport; otherwise
            // it would hold the read half open until the connection drops.
            self.reader_task.abort();
        }
    }

    impl<T: AsyncRead + AsyncWrite + Send + 'static> Transport for Stream<T> {
        async fn send(&mut self, data: Bytes) -> Result<(), Error> {
            // QMux01 frames travel inside size-prefixed records on byte streams.
            // (Records are implicit on WebSocket, where the message boundary delimits them.)
            if self.version == Version::QMux01 {
                let mut size_buf = BytesMut::with_capacity(8);
                VarInt::try_from(data.len())?.encode(&mut size_buf);
                self.writer.write_all(&size_buf).await?;
            }
            self.writer.write_all(&data).await?;
            self.writer.flush().await?;
            Ok(())
        }

        async fn recv(&mut self) -> Result<Bytes, Error> {
            // mpsc::Receiver::recv is cancel safe, so dropping this future never
            // loses a buffered frame. `None` means the reader task exited without
            // sending — treat as a clean close.
            self.rx.recv().await.unwrap_or(Err(Error::Closed))
        }

        async fn close(&mut self) -> Result<(), Error> {
            self.writer.shutdown().await?;
            Ok(())
        }
    }

    /// Reader task: pull complete frames off the wire and ship them through `tx`.
    /// On parse error, send the error and exit. If `tx` is closed (the transport
    /// was dropped), exit silently.
    async fn reader_loop<R: AsyncRead + Unpin>(
        mut reader: BufReader<R>,
        version: Version,
        our_max_record_size: usize,
        tx: mpsc::Sender<Result<Bytes, Error>>,
    ) {
        loop {
            let result = match version {
                Version::QMux01 => recv_record(&mut reader, our_max_record_size).await,
                Version::QMux00 | Version::WebTransport => recv_qmux00_frame(&mut reader).await,
            };
            let stop = result.is_err();
            if tx.send(result).await.is_err() {
                return;
            }
            if stop {
                return;
            }
        }
    }

    /// Read a varint from the stream, returning the decoded value.
    /// If `buf` is provided, appends the raw bytes to it.
    async fn read_varint_into<R: AsyncRead + Unpin>(
        reader: &mut R,
        buf: &mut BytesMut,
    ) -> Result<VarInt, Error> {
        let first = reader.read_u8().await?;
        buf.put_u8(first);

        let tag = first >> 6;
        let len = 1usize << tag;

        if len == 1 {
            return Ok(VarInt::try_from((first & 0x3f) as u64).unwrap());
        }

        let start = buf.len();
        buf.resize(start + len - 1, 0);
        reader.read_exact(&mut buf[start..]).await?;

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

    /// Read a varint from the stream without collecting raw bytes.
    async fn read_varint<R: AsyncRead + Unpin>(reader: &mut R) -> Result<VarInt, Error> {
        let first = reader.read_u8().await?;
        let tag = first >> 6;
        let len = 1usize << tag;

        if len == 1 {
            return Ok(VarInt::try_from((first & 0x3f) as u64).unwrap());
        }

        let mut raw = [0u8; 8];
        raw[0] = first & 0x3f;
        reader.read_exact(&mut raw[1..len]).await?;

        let value = match len {
            2 => u16::from_be_bytes([raw[0], raw[1]]) as u64,
            4 => u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
            8 => u64::from_be_bytes(raw),
            _ => unreachable!(),
        };

        VarInt::try_from(value).map_err(|_| Error::Short)
    }

    /// Read exactly `len` bytes, appending to buf.
    async fn read_bytes<R: AsyncRead + Unpin>(
        reader: &mut R,
        len: usize,
        buf: &mut BytesMut,
    ) -> Result<(), Error> {
        let start = buf.len();
        buf.resize(start + len, 0);
        reader.read_exact(&mut buf[start..]).await?;
        Ok(())
    }

    /// Read one QMux Record from the byte stream (draft-01).
    /// Returns the record payload (frames concatenated).
    async fn recv_record<R: AsyncRead + Unpin>(
        reader: &mut R,
        our_max_record_size: usize,
    ) -> Result<Bytes, Error> {
        let size = read_varint(reader).await?.into_inner() as usize;
        if size > our_max_record_size {
            return Err(Error::FrameTooLarge);
        }
        let mut buf = BytesMut::zeroed(size);
        reader.read_exact(&mut buf).await?;
        Ok(buf.freeze())
    }

    /// Read one complete QMux frame from the byte stream (draft-00), returning raw bytes.
    async fn recv_qmux00_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Bytes, Error> {
        let mut buf = BytesMut::new();
        let frame_type = read_varint_into(reader, &mut buf).await?.into_inner();

        // STREAM frames: 0x08-0x0f
        if (0x08..=0x0f).contains(&frame_type) {
            let has_off = frame_type & 0x04 != 0;
            let has_len = frame_type & 0x02 != 0;

            read_varint_into(reader, &mut buf).await?; // stream id

            if has_off {
                read_varint_into(reader, &mut buf).await?; // offset
            }

            if has_len {
                let len = read_varint_into(reader, &mut buf).await?.into_inner() as usize;
                if len > MAX_FRAME_PAYLOAD {
                    return Err(Error::FrameTooLarge);
                }
                read_bytes(reader, len, &mut buf).await?;
            } else {
                return Err(Error::Short);
            }

            return Ok(buf.freeze());
        }

        match frame_type {
            // PADDING
            0x00 => {}
            // RESET_STREAM
            0x04 => {
                read_varint_into(reader, &mut buf).await?; // id
                read_varint_into(reader, &mut buf).await?; // code
                read_varint_into(reader, &mut buf).await?; // final_size
            }
            // STOP_SENDING
            0x05 => {
                read_varint_into(reader, &mut buf).await?; // id
                read_varint_into(reader, &mut buf).await?; // code
            }
            // CONNECTION_CLOSE / APPLICATION_CLOSE
            0x1c | 0x1d => {
                read_varint_into(reader, &mut buf).await?; // code
                read_varint_into(reader, &mut buf).await?; // frame_type
                let reason_len = read_varint_into(reader, &mut buf).await?.into_inner() as usize;
                if reason_len > MAX_FRAME_SIZE {
                    return Err(Error::FrameTooLarge);
                }
                read_bytes(reader, reason_len, &mut buf).await?;
            }
            // MAX_DATA
            0x10 => {
                read_varint_into(reader, &mut buf).await?;
            }
            // MAX_STREAM_DATA
            0x11 => {
                read_varint_into(reader, &mut buf).await?; // id
                read_varint_into(reader, &mut buf).await?; // max
            }
            // MAX_STREAMS (bidi/uni)
            0x12 | 0x13 => {
                read_varint_into(reader, &mut buf).await?;
            }
            // DATA_BLOCKED
            0x14 => {
                read_varint_into(reader, &mut buf).await?;
            }
            // STREAM_DATA_BLOCKED
            0x15 => {
                read_varint_into(reader, &mut buf).await?; // id
                read_varint_into(reader, &mut buf).await?; // limit
            }
            // STREAMS_BLOCKED (bidi/uni)
            0x16 | 0x17 => {
                read_varint_into(reader, &mut buf).await?;
            }
            // DATAGRAM without length — can't delimit on a byte stream
            0x30 => return Err(Error::InvalidFrameType(frame_type)),
            // DATAGRAM with length
            0x31 => {
                let len = read_varint_into(reader, &mut buf).await?.into_inner() as usize;
                if len > MAX_FRAME_SIZE {
                    return Err(Error::FrameTooLarge);
                }
                read_bytes(reader, len, &mut buf).await?;
            }
            // QX_TRANSPORT_PARAMETERS
            0x3f5153300d0a0d0a => {
                let len = read_varint_into(reader, &mut buf).await?.into_inner() as usize;
                if len > MAX_FRAME_SIZE {
                    return Err(Error::FrameTooLarge);
                }
                read_bytes(reader, len, &mut buf).await?;
            }
            // QX_PING request/response (also valid in draft-00 for forward compat)
            0x348c67529ef8c7bd | 0x348c67529ef8c7be => {
                read_varint_into(reader, &mut buf).await?; // sequence
            }
            _ => return Err(Error::InvalidFrameType(frame_type)),
        }

        Ok(buf.freeze())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::io::AsyncWriteExt;

        // Drip a frame in one byte at a time, racing each `recv` against an
        // immediate yield so the recv future is dropped every iteration. With
        // a cancel-safe `recv`, the final call must still return the whole
        // frame intact.
        #[tokio::test]
        async fn recv_is_cancel_safe_across_partial_writes() {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            let mut transport = Stream::new(client, Version::QMux00, 16 * 1024);

            // STREAM frame, type 0x0a (len bit set), id=4, length=5, payload="hello".
            let mut frame = Vec::new();
            frame.push(0x0a);
            frame.push(0x04);
            frame.push(0x05);
            frame.extend_from_slice(b"hello");

            for chunk in frame.chunks(1).take(frame.len() - 1) {
                server.write_all(chunk).await.unwrap();
                server.flush().await.unwrap();
                tokio::select! {
                    _ = transport.recv() => panic!("recv completed with a partial frame"),
                    _ = tokio::task::yield_now() => {}
                }
            }

            server.write_all(&frame[frame.len() - 1..]).await.unwrap();
            server.flush().await.unwrap();

            let got = transport.recv().await.expect("frame should decode");
            assert_eq!(&got[..], frame.as_slice());
        }

        #[tokio::test]
        async fn recv_qmux01_record_is_cancel_safe() {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            let mut transport = Stream::new(client, Version::QMux01, 16 * 1024);

            // 1-byte varint length (0x08) followed by 8 bytes of payload.
            let mut record = vec![0x08];
            record.extend_from_slice(b"abcdefgh");

            for chunk in record.chunks(1).take(record.len() - 1) {
                server.write_all(chunk).await.unwrap();
                server.flush().await.unwrap();
                tokio::select! {
                    _ = transport.recv() => panic!("recv completed with a partial record"),
                    _ = tokio::task::yield_now() => {}
                }
            }

            server.write_all(&record[record.len() - 1..]).await.unwrap();
            server.flush().await.unwrap();

            let got = transport.recv().await.expect("record should decode");
            assert_eq!(&got[..], b"abcdefgh");
        }

        // Two frames arrive in a single write. Each recv() must return one
        // complete frame, in order. Exercises the channel queue + the reader
        // task looping on a buffer that still has bytes after parsing.
        #[tokio::test]
        async fn recv_returns_consecutive_frames_in_order() {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            let mut transport = Stream::new(client, Version::QMux00, 16 * 1024);

            // Two STREAM frames (type 0x0a) for stream ids 4 and 8.
            let frame_a: Vec<u8> = [0x0a, 0x04, 0x05].into_iter().chain(*b"hello").collect();
            let frame_b: Vec<u8> = [0x0a, 0x08, 0x05].into_iter().chain(*b"world").collect();
            let mut combined = frame_a.clone();
            combined.extend_from_slice(&frame_b);

            server.write_all(&combined).await.unwrap();
            server.flush().await.unwrap();

            let got_a = transport.recv().await.expect("first frame should decode");
            let got_b = transport.recv().await.expect("second frame should decode");
            assert_eq!(&got_a[..], frame_a.as_slice());
            assert_eq!(&got_b[..], frame_b.as_slice());
        }

        // Reader task hits a parse error: `recv()` returns it, and the next
        // `recv()` returns Error::Closed since the task has exited.
        #[tokio::test]
        async fn recv_propagates_parse_error_then_closes() {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            let mut transport = Stream::new(client, Version::QMux00, 16 * 1024);

            // Frame type 0x02 isn't a recognized QMux00 frame type.
            server.write_all(&[0x02]).await.unwrap();
            server.flush().await.unwrap();

            let err = transport.recv().await.expect_err("parse error expected");
            assert!(matches!(err, Error::InvalidFrameType(0x02)), "got {err:?}");

            // Task has exited after sending the error; subsequent recv sees the
            // closed channel and reports Error::Closed.
            let err = transport.recv().await.expect_err("closed expected");
            assert!(matches!(err, Error::Closed), "got {err:?}");
        }

        // A record whose declared size exceeds `our_max_record_size` is
        // rejected with FrameTooLarge before any payload is consumed.
        #[tokio::test]
        async fn recv_record_exceeding_max_returns_frame_too_large() {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            let mut transport = Stream::new(client, Version::QMux01, 4);

            // 1-byte varint length = 5, which exceeds the configured max of 4.
            server.write_all(&[0x05]).await.unwrap();
            server.flush().await.unwrap();

            let err = transport.recv().await.expect_err("FrameTooLarge expected");
            assert!(matches!(err, Error::FrameTooLarge), "got {err:?}");
        }
    }
}

#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
pub use stream_transport::Stream;

// Shared plumbing for the byte-stream transports (TCP, Unix sockets).
#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
mod stream_session {
    use tokio::io::{AsyncRead, AsyncWrite};

    use super::Stream;
    use crate::protocol::validate_protocol;
    use crate::{Config, Error, Protocol, Session};

    /// Wrap a byte stream in a [`Stream`] and start a session, validating any
    /// advertised protocol names first. Used by the `tcp`/`uds` builders.
    pub(crate) fn build<T: AsyncRead + AsyncWrite + Send + 'static>(
        stream: T,
        config: Config,
        is_server: bool,
    ) -> Result<Session, Error> {
        if let Protocol::Negotiate(protocols) = &config.protocol {
            for protocol in protocols {
                validate_protocol(protocol)?;
            }
        }
        let transport = Stream::new(stream, config.version, config.max_record_size);
        Ok(if is_server {
            Session::accept(transport, config)
        } else {
            Session::connect(transport, config)
        })
    }
}

#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
pub(crate) use stream_session::build as build_stream_session;

// WsTransport: message I/O over WebSocket.
#[cfg(feature = "ws")]
mod ws_transport {
    use std::pin::Pin;
    use std::time::Duration;

    use bytes::Bytes;
    use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};
    use tokio_tungstenite::tungstenite;

    use super::Transport;
    use crate::ws::KeepAlive;
    use crate::Error;

    pub(crate) struct WsTransport<T> {
        ws: T,
        keep_alive: Option<KeepAliveState>,
    }

    struct KeepAliveState {
        // Fires on each interval; we send a Ping when it does.
        interval: Interval,

        // Resets every time we receive a frame. If it elapses, the peer is gone.
        deadline: Pin<Box<Sleep>>,

        timeout: Duration,
    }

    impl KeepAliveState {
        fn new(config: KeepAlive) -> Self {
            // tokio::time::interval panics on a zero Duration, and a deadline shorter than the
            // interval would fire before the first ping. Floor both to 1ms so a misconfigured
            // KeepAlive degrades into "very chatty" instead of crashing.
            let interval_dur = config.interval.max(Duration::from_millis(1));
            let timeout = config.timeout.max(interval_dur);

            // Skip catch-up bursts after a long pause; we just want one Ping per tick.
            let mut interval = tokio::time::interval(interval_dur);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            // First tick fires immediately by default; consume it so we don't ping on connect.
            interval.reset();

            Self {
                interval,
                deadline: Box::pin(tokio::time::sleep(timeout)),
                timeout,
            }
        }

        fn observe_recv(&mut self) {
            self.deadline.as_mut().reset(Instant::now() + self.timeout);
        }
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
            Self {
                ws,
                keep_alive: None,
            }
        }

        pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
            self.keep_alive = Some(KeepAliveState::new(keep_alive));
            self
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
            use futures::{SinkExt, StreamExt};

            // Destructure so we can take separate &mut borrows of `ws` and `keep_alive`.
            let Self { ws, keep_alive } = self;

            loop {
                enum Event<M> {
                    Message(M),
                    SendPing,
                    Timeout,
                }

                let event = match keep_alive {
                    Some(ka) => tokio::select! {
                        msg = ws.next() => Event::Message(msg),
                        _ = ka.interval.tick() => Event::SendPing,
                        _ = ka.deadline.as_mut() => Event::Timeout,
                    },
                    None => Event::Message(ws.next().await),
                };

                let message = match event {
                    Event::Message(msg) => msg.ok_or(Error::Closed)??,
                    Event::SendPing => {
                        ws.send(tungstenite::Message::Ping(Bytes::new()))
                            .await
                            .map_err(|_| Error::Closed)?;
                        continue;
                    }
                    Event::Timeout => {
                        tracing::debug!("websocket keep_alive timeout");
                        return Err(Error::Closed);
                    }
                };

                if let Some(ka) = keep_alive.as_mut() {
                    ka.observe_recv();
                }

                match message {
                    tungstenite::Message::Binary(data) => {
                        return Ok(data);
                    }
                    tungstenite::Message::Close(_) => {
                        return Err(Error::Closed);
                    }
                    tungstenite::Message::Ping(_)
                    | tungstenite::Message::Pong(_)
                    | tungstenite::Message::Text(_)
                    | tungstenite::Message::Frame(_) => {
                        // tungstenite auto-queues a Pong reply when it reads a Ping;
                        // it gets flushed on our next send/read. No manual reply needed.
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
